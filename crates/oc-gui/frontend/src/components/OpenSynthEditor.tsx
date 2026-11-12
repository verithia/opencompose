import type { ParameterRequestOwner } from "../stores/parameterAdmission";
import { For, Show, createEffect, createMemo, createSignal, onCleanup, untrack } from "solid-js";
import { Portal } from "solid-js/web";
import "../styles/synth.css";
import type { Effect, ParamValue, PresetDef } from "../types/project";
import { Dropdown } from "./Dropdown";
import { EffectsRack } from "./EffectsRack";
import { FactoryPatchBrowser, type FactoryPatchInfo } from "./FactoryPatchBrowser";
import { PatchActionDialog } from "./PatchActionDialog";
import { EXTRA_PRESETS } from "../data/factoryPresets";
import { SynthFxEditor } from "./SynthFxEditor";
import { SynthWaveDisplay } from "./synth/SynthWaveDisplay";
import { SynthParameterButton } from "./synth/SynthParameterButton";
import { parameterFromPosition, parameterKeyValue, parameterPosition, parameterValue } from "./synth/numericRange";
import { SYNTH_CATALOG, enumOptions, paramLabel, specForConcreteKey } from "../data/synthCatalog";
import { openContextMenu, type ContextMenuItem } from "./ContextMenu";
import {
  automationLabel,
  automationLaneKey,
  automationLaneName,
  automationTarget,
  type AutomateTarget,
} from "../utils/automationTarget";
import {
  automationReadout,
  unwatchAutomation,
  watchAutomation,
} from "../stores/automationReadout";
import { store, type AbCandidateInput } from "../stores/project";
import { importExternalPreset } from "../stores/api";
import { beginParameterGesture, endOwnedUndoGesture, exportPatchBundle, importPatchBundle, type AbSnapshot, type UndoGestureToken } from "../stores/api";
import { isSecondaryClick } from "../stores/gesture";
import {
  automateOwnedSynthParamAt,
  beginGuardedSynthParamGesture,
  endOwnedSynthParamGesture,
  removeSynthParamAutomation,
  updateOwnedSynthParamGesture,
  type AutomationPosition,
} from "../stores/synthAutomation";
import {
  automationWriteArmed,
  closeAutomationWrite,
  endAutomationWrite,
  toggleAutomationWriteArm,
  writeAutomationOnce,
  type AutomationWriteHandle,
  type AutomationWriteSurface,
} from "../stores/automationWrite";
import { createParameterGesture } from "../utils/parameterGesture";
import { automationPositionAt } from "../utils/automationSampler";
import { i18n, type I18nKey } from "../stores/i18n";
import { log } from "../stores/log";
import type { MacroLearnResult } from "../stores/api";
import { useTooltip, type TooltipAttachment } from "./Tooltip";
import { synthOptionLabel } from "../utils/synthOptionLabel";
import { ControlLinkEditor } from "./ControlLinkEditor";
import { rememberTweaked } from "../stores/controlLinks";
import { formatParameterValue } from "../utils/parameterUnits";

function formatModAmount(amount: number): string {
  const spec = specForConcreteKey("mod.1.amount")!;
  return formatParameterValue(amount, spec.step, spec.unit);
}

type View = "sound" | "mod" | "fx" | "voice";
type SoundPage = "osc" | "filter" | "amp";
type LearnMessage = { key: I18nKey; values?: Record<string, string | number> };

// Every list below is generated from the parameter catalog, so an engine
// enum id cannot exist without a matching dropdown entry.
const WAVEFORMS = SYNTH_CATALOG.waveforms.options
  .filter((option) => option.status === "working")
  .map((option) => option.label);
// Every oscillator can stack up to sixteen voices; the engine clamps to the same.
const UNISON_VOICES = Array.from({ length: 16 }, (_, index) => [index + 1, `${index + 1}v`] as [number, string]);
// No-op warp ids stay visible and carry a status suffix instead of vanishing.
const WARP_MODES = enumOptions("warpMode", true).map((option) => option.label);
const FILTER_MODES = enumOptions("filterMode").map((option) => option.label);
const FILTER_MODELS = enumOptions("filterModel").map((option) => option.label);
const MOD_SOURCES = enumOptions("modSource").map((option) => option.label);
const MOD_DESTINATIONS = enumOptions("modDestination").map((option) => option.label);
const MOD_AUX = enumOptions("modAux").map((option) => option.label);
/** Control → destination pairs are generated from the Rust catalog. */
const MOD_DESTINATION_BY_CONTROL = new Map(
  SYNTH_CATALOG.modLearnTargets.map((target) => [target.control, target.destination]),
);
const LFO_SHAPES = enumOptions("lfoShape").map((option) => option.label);
const LFO_COUNT = 10;
const MATRIX_SLOTS = 64;
const MATRIX_PAGE_SIZE = 16;
const MATRIX_PAGES = MATRIX_SLOTS / MATRIX_PAGE_SIZE;
// 43 sync divisions matching the engine's ladder; entry 22 is a quarter note.
const LFO_DIVISIONS = (() => {
  const names = [
    "32 bars", "16 bars", "8 bars", "4 bars", "2 bars", "1 bar",
    "1/2", "1/4", "1/8", "1/16", "1/32", "1/64", "1/128", "1/256",
  ];
  const entries: [number, string][] = [];
  let base = 128;
  for (const name of names) {
    entries.push([base * 1.5, `${name} dotted`], [base, name], [(base * 2) / 3, `${name} triplet`]);
    base /= 2;
  }
  entries.push([0.0078125, "1/512"]);
  entries.sort((a, b) => b[0] - a[0]);
  return entries.map(([, label]) => label);
})();

/** LFO 1 keeps the original `lfo.*` contract; LFO 2-10 write `lfoN.*`. */
function lfoParamName(index: number, field: string): string {
  return index === 1 ? `lfo.${field}` : `lfo${index}.${field}`;
}
export const AUDITION_KEYS = [
  [60, "C4"], [61, "C#4"], [62, "D4"], [63, "D#4"], [64, "E4"], [65, "F4"],
  [66, "F#4"], [67, "G4"], [68, "G#4"], [69, "A4"], [70, "A#4"], [71, "B4"], [72, "C5"],
] as const;

/** Whole-patch batch the backend read from an external preset file. */
const EXTERNAL_PRESET_DIR = "opencompose.externalPresetDir";

/** Folder of the last import, so the picker reopens where the user left it. */
function rememberedPresetDir(): string | undefined {
  try {
    return window.localStorage.getItem(EXTERNAL_PRESET_DIR) ?? undefined;
  } catch {
    return undefined;
  }
}

function rememberPresetDir(path: string) {
  const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  if (cut <= 0) return;
  try {
    window.localStorage.setItem(EXTERNAL_PRESET_DIR, path.slice(0, cut));
  } catch {
    // Storage can be unavailable; the picker still works without a default.
  }
}

const FACTORY_PATCHES: Record<string, [string, string | null][]> = {
  Init: [
    ["synth_version", "2"], ["osc.a.waveform", "1"], ["osc.a.level", "0.78"], ["osc.a.semitones", "0"],
    ["osc.a.pulse_width", "0.5"], ["osc.a.wavetable_position", "0"], ["osc.a.warp_mode", "0"], ["osc.a.warp_amount", "0"],
    ["osc.a.warp_var", "0"], ["osc.a.warp2_mode", "0"], ["osc.a.warp2_amount", "0"], ["osc.a.warp2_var", "0"],
    ["osc.a.pan", "0"],
    ["osc.b.waveform", "2"], ["osc.b.level", "0"], ["osc.b.semitones", "0"], ["osc.b.pulse_width", "0.5"], ["osc.b.wavetable_position", "0"], ["osc.b.warp_mode", "0"], ["osc.b.warp_amount", "0"],
    ["osc.b.warp_var", "0"], ["osc.b.warp2_mode", "0"], ["osc.b.warp2_amount", "0"], ["osc.b.warp2_var", "0"],
    ["osc.b.unison", "1"], ["osc.b.detune", "0"], ["osc.b.width", "0"], ["osc.b.pan", "0"],
    ["osc.c.waveform", "1"], ["osc.c.level", "0"], ["osc.c.semitones", "0"], ["osc.c.pulse_width", "0.5"], ["osc.c.wavetable_position", "0"], ["osc.c.warp_mode", "0"], ["osc.c.warp_amount", "0"],
    ["osc.c.warp_var", "0"], ["osc.c.warp2_mode", "0"], ["osc.c.warp2_amount", "0"], ["osc.c.warp2_var", "0"],
    ["osc.c.unison", "1"], ["osc.c.detune", "0"], ["osc.c.width", "0"], ["osc.c.pan", "0"],
    ["sub.level", "0"], ["sub.waveform", "0"], ["sub.semitones", "-12"],
    ["sub.fine", "0"], ["sub.pan", "0"], ["sub.initial_phase", "0"], ["sub.contiguous_phase", "0"],
    ["noise.level", "0"], ["ring.level", "0"],
    ["unison.voices", "2"], ["unison.detune", "7"], ["unison.width", "0.55"], ["output.gain", "0.22"],
    ["filter.mode", "0"], ["filter.model", "0"], ["filter.cutoff", "12000"], ["filter.resonance", "0.15"], ["filter.drive", "0"], ["filter.wet", "1"], ["filter.keytrack", "0.25"], ["filter.keytrack.origin", "60"], ["filter.env.amount", "0"],
    ["filter.routing", "0"],
    ["filter2.enabled", "0"], ["filter2.mode", "0"], ["filter2.model", "0"], ["filter2.cutoff", "12000"], ["filter2.resonance", "0"], ["filter2.drive", "0"], ["filter2.wet", "1"],
    ["filter_env.attack", "0.01"], ["filter_env.decay", "0.2"], ["filter_env.sustain", "0"], ["filter_env.release", "0.25"], ["filter_env.curve", "0"],
    ["amp.attack", "0.008"], ["amp.decay", "0.16"], ["amp.sustain", "0.72"], ["amp.release", "0.28"], ["amp.curve", "0"],
    ["velocity.to_amp", "1"],
    ["voice.mono", "0"], ["voice.polyphony", "16"], ["voice.legato", "1"], ["voice.glide", "0.08"], ["quality.oversampling", "1"],
    ["osc.start_phase", "0"], ["osc.random_phase", "0"],
    ["tuning.reference_hz", "440"], ["tuning.transpose", "0"], ["tuning.fine", "0"],
    ["lfo.shape", "0"], ["lfo.retrigger", "0"], ["lfo.sync", "0"], ["lfo.division", "2"], ["lfo.sync_div", "22"], ["lfo.rate", "5.2"], ["lfo.pitch", "0"],
    ["lfo.smooth", "0"], ["lfo.rise", "0"], ["lfo.delay", "0"], ["lfo.bend", "0"], ["lfo.phase", "0"], ["lfo.rate_x10", "0"],
    ["env3.attack", "0.008"], ["env3.decay", "0.16"], ["env3.sustain", "0.72"], ["env3.release", "0.28"], ["env3.curve", "0"],
    ["env4.attack", "0.008"], ["env4.decay", "0.16"], ["env4.sustain", "0.72"], ["env4.release", "0.28"], ["env4.curve", "0"],
    ["lfo2.shape", "0"], ["lfo2.retrigger", "0"], ["lfo2.sync", "0"], ["lfo2.division", "2"], ["lfo2.sync_div", "22"], ["lfo2.rate", "5.2"],
    ["lfo2.smooth", "0"], ["lfo2.rise", "0"], ["lfo2.delay", "0"], ["lfo2.bend", "0"], ["lfo2.phase", "0"], ["lfo2.rate_x10", "0"],
    ["lfo3.shape", "0"], ["lfo3.retrigger", "0"], ["lfo3.sync", "0"], ["lfo3.division", "2"], ["lfo3.sync_div", "22"], ["lfo3.rate", "5.2"],
    ["lfo3.smooth", "0"], ["lfo3.rise", "0"], ["lfo3.delay", "0"], ["lfo3.bend", "0"], ["lfo3.phase", "0"], ["lfo3.rate_x10", "0"],
    ["lfo4.shape", "0"], ["lfo4.retrigger", "0"], ["lfo4.sync", "0"], ["lfo4.division", "2"], ["lfo4.sync_div", "22"], ["lfo4.rate", "5.2"],
    ["lfo4.smooth", "0"], ["lfo4.rise", "0"], ["lfo4.delay", "0"], ["lfo4.bend", "0"], ["lfo4.phase", "0"], ["lfo4.rate_x10", "0"],
    ["lfo5.shape", "0"], ["lfo5.retrigger", "0"], ["lfo5.sync", "0"], ["lfo5.division", "2"], ["lfo5.sync_div", "22"], ["lfo5.rate", "5.2"],
    ["lfo5.smooth", "0"], ["lfo5.rise", "0"], ["lfo5.delay", "0"], ["lfo5.bend", "0"], ["lfo5.phase", "0"], ["lfo5.rate_x10", "0"],
    ["lfo6.shape", "0"], ["lfo6.retrigger", "0"], ["lfo6.sync", "0"], ["lfo6.division", "2"], ["lfo6.sync_div", "22"], ["lfo6.rate", "5.2"],
    ["lfo6.smooth", "0"], ["lfo6.rise", "0"], ["lfo6.delay", "0"], ["lfo6.bend", "0"], ["lfo6.phase", "0"], ["lfo6.rate_x10", "0"],
    ["lfo7.shape", "0"], ["lfo7.retrigger", "0"], ["lfo7.sync", "0"], ["lfo7.division", "2"], ["lfo7.sync_div", "22"], ["lfo7.rate", "5.2"],
    ["lfo7.smooth", "0"], ["lfo7.rise", "0"], ["lfo7.delay", "0"], ["lfo7.bend", "0"], ["lfo7.phase", "0"], ["lfo7.rate_x10", "0"],
    ["lfo8.shape", "0"], ["lfo8.retrigger", "0"], ["lfo8.sync", "0"], ["lfo8.division", "2"], ["lfo8.sync_div", "22"], ["lfo8.rate", "5.2"],
    ["lfo8.smooth", "0"], ["lfo8.rise", "0"], ["lfo8.delay", "0"], ["lfo8.bend", "0"], ["lfo8.phase", "0"], ["lfo8.rate_x10", "0"],
    ["lfo9.shape", "0"], ["lfo9.retrigger", "0"], ["lfo9.sync", "0"], ["lfo9.division", "2"], ["lfo9.sync_div", "22"], ["lfo9.rate", "5.2"],
    ["lfo9.smooth", "0"], ["lfo9.rise", "0"], ["lfo9.delay", "0"], ["lfo9.bend", "0"], ["lfo9.phase", "0"], ["lfo9.rate_x10", "0"],
    ["lfo10.shape", "0"], ["lfo10.retrigger", "0"], ["lfo10.sync", "0"], ["lfo10.division", "2"], ["lfo10.sync_div", "22"], ["lfo10.rate", "5.2"],
    ["lfo10.smooth", "0"], ["lfo10.rise", "0"], ["lfo10.delay", "0"], ["lfo10.bend", "0"], ["lfo10.phase", "0"], ["lfo10.rate_x10", "0"],
    ["wavetable.file", null], ["open_synth_state_file", null],
    ["step.rate", "4"], ["step.sync", "1"], ["step.division", "0"], ["step.smoothing", "0"],
  ],
  "Neon Bass": [
    ["synth_version", "2"], ["osc.a.waveform", "1"], ["osc.a.level", "0.74"], ["osc.b.waveform", "2"],
    ["osc.b.level", "0.22"], ["osc.b.semitones", "-12"], ["sub.level", "0.42"], ["noise.level", "0.01"],
    ["filter.cutoff", "740"], ["filter.resonance", "0.32"], ["filter.env.amount", "3.8"], ["filter.keytrack", "0.18"],
    ["filter_env.attack", "0.004"], ["filter_env.decay", "0.24"], ["filter_env.sustain", "0.08"], ["filter_env.release", "0.18"],
    ["amp.attack", "0.004"], ["amp.decay", "0.18"], ["amp.sustain", "0.62"], ["amp.release", "0.14"],
    ["voice.mono", "1"], ["voice.legato", "1"], ["voice.glide", "0.07"], ["output.gain", "0.26"],
  ],
  "Glass Pad": [
    ["synth_version", "2"], ["osc.a.waveform", "4"], ["osc.a.wavetable_position", "0.58"], ["osc.a.level", "0.58"], ["osc.b.waveform", "0"],
    ["osc.b.level", "0.32"], ["osc.b.semitones", "12"], ["noise.level", "0.035"], ["unison.detune", "14"],
    ["filter.cutoff", "4200"], ["filter.resonance", "0.2"], ["filter.env.amount", "1.4"],
    ["amp.attack", "0.72"], ["amp.decay", "1.4"], ["amp.sustain", "0.78"], ["amp.release", "3.2"],
    ["voice.mono", "0"], ["lfo.sync", "1"], ["lfo.sync_div", "16"], ["mod.1.source", "0"], ["mod.1.destination", "4"], ["mod.1.amount", "0.2"],
  ],
  "Pulse Pluck": [
    ["synth_version", "2"], ["osc.a.waveform", "2"], ["osc.a.level", "0.68"], ["osc.b.waveform", "1"], ["osc.b.level", "0.17"],
    ["filter.cutoff", "1800"], ["filter.resonance", "0.44"], ["filter.env.amount", "4.6"],
    ["filter_env.attack", "0"], ["filter_env.decay", "0.16"], ["filter_env.sustain", "0"], ["filter_env.release", "0.12"],
    ["amp.attack", "0"], ["amp.decay", "0.22"], ["amp.sustain", "0.12"], ["amp.release", "0.18"], ["voice.mono", "0"],
  ],
};

// The authored presets are plain overrides on Init, so they join the same table the
// loader reads and inherit its macro/step/modulation resets.
const FACTORY_PATCH_INFO = new Map<string, FactoryPatchInfo>();
for (const preset of EXTRA_PRESETS) {
  FACTORY_PATCHES[preset.name] = preset.params;
  FACTORY_PATCH_INFO.set(preset.name, { name: preset.name, category: preset.category });
}

function factoryParams(name: string): [string, string | null][] {
  const resetModulation: [string, string][] = [];
  // An empty rack keeps a preset without FX from inheriting the last one's chain.
  resetModulation.push(["fx.count", "0"]);
  for (let macro = 1; macro <= 8; macro += 1) resetModulation.push([`macro.${macro}`, "0"]);
  for (let step = 1; step <= 16; step += 1) resetModulation.push([`step.${step}`, "0"]);
  // Init carries the env 3/4 and LFO 2–10 defaults; every one of the 64 matrix
  // slots still needs an explicit reset so stale routes never survive a load.
  for (let route = 1; route <= MATRIX_SLOTS; route += 1) {
    resetModulation.push([`mod.${route}.source`, "0"], [`mod.${route}.destination`, "0"], [`mod.${route}.amount`, "0"]);
  }
  const merged = new Map<string, string | null>();
  for (const [key, value] of [...FACTORY_PATCHES.Init, ...resetModulation, ...(FACTORY_PATCHES[name] ?? [])]) {
    merged.set(key, value);
  }
  return [...merged];
}

/** Browser rows: the factory bank first, then the user library. */
function factoryPatchInfos(library: FactoryPatchInfo[]): FactoryPatchInfo[] {
  return [
    ...Object.keys(FACTORY_PATCHES).map((name) => FACTORY_PATCH_INFO.get(name) ?? { name }),
    ...library,
  ];
}

function paramText(value: ParamValue): string {
  if ("Number" in value) return String(value.Number);
  if ("Db" in value) return String(value.Db);
  if ("Percentage" in value) return String(value.Percentage);
  if ("Frequency" in value) return String(value.Frequency);
  if ("Cents" in value) return String(value.Cents);
  if ("Time" in value) return String(value.Time);
  if ("String" in value) return value.String;
  if ("Bool" in value) return value.Bool ? "1" : "0";
  return "";
}

function storedValue(preset: PresetDef, name: string, fallback: number): number {
  const found = preset.params.find(([key]) => key === name);
  const value = found ? Number.parseFloat(paramText(found[1])) : Number.NaN;
  return Number.isFinite(value) ? value : fallback;
}

function storedArray(preset: PresetDef, name: string): number[] {
  const found = preset.params.find(([key]) => key === name)?.[1];
  if (!found) return [];
  if ("Array" in found) return found.Array;
  if ("String" in found) {
    return found.String.split(",").map(Number).filter(Number.isFinite);
  }
  return [];
}

function storedString(preset: PresetDef, name: string): string | null {
  const found = preset.params.find(([key]) => key === name)?.[1];
  return found && "String" in found ? found.String : null;
}

function EnvelopeDisplay(props: { attack: number; decay: number; sustain: number; release: number; curve: number; label: string }) {
  const points = createMemo(() => {
    const total = Math.max(0.1, props.attack + props.decay + props.release + 0.5);
    const attackX = Math.min(32, props.attack / total * 100);
    const decayX = Math.min(64, attackX + props.decay / total * 100);
    const releaseX = Math.max(decayX + 12, 100 - Math.min(32, props.release / total * 100));
    const sustainY = 46 - props.sustain * 38;
    const curveOffset = props.curve * 7;
    return `M0 46 Q${attackX * 0.5} ${27 + curveOffset} ${attackX} 8 Q${(attackX + decayX) * 0.5} ${sustainY - curveOffset} ${decayX} ${sustainY} L${releaseX} ${sustainY} Q${(releaseX + 100) * 0.5} ${27 + curveOffset} 100 46`;
  });
  return (
    <svg class="synth-envelope-display" viewBox="0 0 100 52" role="img" aria-label={i18n.t("synth.ui.envelopeDisplay", { name: props.label })}>
      <path class="synth-graph-grid" d="M0 46H100 M25 0V52 M50 0V52 M75 0V52" />
      <path class="synth-graph-line" d={points()} />
    </svg>
  );
}

function FilterDisplay(props: { cutoff: number; resonance: number; mode: number; model?: number; label?: string }) {
  const path = createMemo(() => {
    const x = Math.max(5, Math.min(94, Math.log(props.cutoff / 20) / Math.log(1000) * 100));
    const bump = 4 + props.resonance * 18;
    if ((props.model ?? 0) > 0) return `M0 10 L${Math.max(0, x - 20)} 10 Q${x - 3} ${10 - bump} ${x + 4} 16 L100 46`;
    if (props.mode === 1) return `M0 44 Q${x - 12} 42 ${x} ${10 - bump} Q${x + 12} 42 100 44`;
    if (props.mode === 2) return `M0 44 Q${Math.max(0, x - 8)} 38 ${x} 10 L100 10`;
    if (props.mode === 3) return `M0 10 L${Math.max(0, x - 11)} 10 Q${x} ${44 + bump * 0.25} ${Math.min(100, x + 11)} 10 L100 10`;
    return `M0 10 L${Math.max(0, x - 14)} 10 Q${x - 3} ${10 - bump} ${x} 10 Q${x + 8} 38 100 44`;
  });
  return (
    <svg class="synth-filter-display" viewBox="0 0 100 52" role="img" aria-label={i18n.t("synth.ui.filterResponse", { name: props.label ?? "", model: synthOptionLabel(FILTER_MODELS[props.model ?? 0] ?? FILTER_MODELS[0]), mode: synthOptionLabel(FILTER_MODES[props.mode] ?? FILTER_MODES[0]) }).trim()}>
      <path class="synth-graph-grid" d="M0 10H100 M0 26H100 M0 44H100 M25 0V52 M50 0V52 M75 0V52" />
      <path class="synth-graph-line" d={path()} />
    </svg>
  );
}

/** Sample the drawn curve the same way the engine does (bent segments). */
function lfoCurveAt(xs: number[], ys: number[], curves: number[], count: number, phase: number): number {
  const points = Math.max(2, Math.min(count, xs.length, ys.length));
  if (phase >= xs[points - 1] || phase < xs[0]) {
    const last = xs[points - 1];
    const span = xs[0] + 1 - last;
    const u = span <= 0 ? 0 : (phase >= last ? (phase - last) / span : (phase + 1 - last) / span);
    return ys[points - 1] + (ys[0] - ys[points - 1]) * u;
  }
  let index = 0;
  while (index < points - 2 && xs[index + 1] <= phase) index += 1;
  const span = xs[index + 1] - xs[index];
  const u = span <= 0 ? 0 : (phase - xs[index]) / span;
  const bend = Math.min(1, Math.max(0, curves[index] ?? 0.5));
  const shaped = bend === 0.5 ? u : u ** (4 ** (2 * bend - 1));
  return ys[index] + (ys[index + 1] - ys[index]) * shaped;
}

function generatorPreview(kind: number): number[] {
  // Illustrative contours for the generated shapes; audio comes from the engine.
  let state = 0x2545f491 ^ (kind * 0x9e3779b9);
  let smooth = 0.5;
  const values: number[] = [];
  for (let index = 0; index < 128; index += 1) {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    const noise = (state >>> 0) / 0xffffffff;
    if (kind === 2) smooth = 0.75 * smooth + 0.25 * noise;
    else if (kind === 3) smooth = 0.55 * smooth + 0.45 * noise;
    else smooth = index % 16 === 0 ? noise : smooth;
    values.push(smooth);
  }
  return values;
}

function LfoCurveDisplay(props: {
  shapeType: number;
  xVals: number[];
  yVals: number[];
  curveVals: number[];
  points: number;
  label: string;
}) {
  const points = createMemo(() => {
    const values: string[] = [];
    if (props.shapeType >= 2) {
      const generated = generatorPreview(props.shapeType);
      for (let index = 0; index < generated.length; index += 1) {
        values.push(`${(index / (generated.length - 1)) * 100},${46 - generated[index] * 36}`);
      }
      return values.join(" ");
    }
    const xs = props.xVals;
    const ys = props.yVals;
    if (xs.length < 2 || ys.length < 2) return "0,46 100,46";
    for (let index = 0; index <= 120; index += 1) {
      const phase = index / 120;
      const y = lfoCurveAt(xs, ys, props.curveVals, props.points, phase);
      values.push(`${phase * 100},${46 - Math.min(1, Math.max(0, y)) * 36}`);
    }
    return values.join(" ");
  });
  return (
    <svg class="synth-lfo-display" viewBox="0 0 100 52" role="img"
      aria-label={i18n.t("synth.ui.lfoShapeDisplay", { name: props.label, shape: synthOptionLabel(LFO_SHAPES[props.shapeType] ?? LFO_SHAPES[0]) })}>
      <path class="synth-graph-grid" d="M0 46H100 M25 0V52 M50 0V52 M75 0V52" />
      <polyline class="synth-graph-line" points={points()} />
    </svg>
  );
}

/** Source groups for the route graph, in MOD_SOURCES index space. */
const SOURCE_KIND = (index: number): "lfo" | "env" | "macro" | "perf" => {
  const name = MOD_SOURCES[index];
  if (name.startsWith("LFO")) return "lfo";
  if (name.includes("Env")) return "env";
  if (name.startsWith("Macro")) return "macro";
  return "perf";
};
/** Destination groups: oscillators / filters / mix & width. */
const DEST_KIND = (index: number): "osc" | "filter" | "mix" => {
  const name = MOD_DESTINATIONS[index] ?? "";
  if (/Filter|Resonance/.test(name)) return "filter";
  if (/Pitch|Pulse|Wavetable|Oscillator|Warp|Source/.test(name)) return "osc";
  return "mix";
};
const DEST_ORDER = { osc: 0, filter: 1, mix: 2 } as const;
const SOURCE_ORDER = { lfo: 0, env: 1, macro: 2, perf: 3 } as const;

type ModRoute = { slot: number; source: number; destination: number; amount: number };

/** Mini routing graph for the modulation matrix: participating sources on the
 * left, destinations on the right, one bezier per active slot. Width and
 * opacity encode |amount|; positive is solid accent, negative dashed cool. */
function ModRouteGraph(props: {
  routes: ModRoute[];
  selected: number | null;
  /** Destination hovered on a modulated control, whose edges light up. */
  highlight?: number | null;
  onSelect: (slot: number) => void;
  onAdd: () => void;
}) {
  const NODE_H = 20;
  const GAP = 4;
  const PAD = 6;
  const SRC_W = 104;
  const DST_W = 150;
  const WIDTH = 760;
  const DST_X = WIDTH - PAD - DST_W;

  // Barycentre ordering: each column is sorted by the mean position of its
  // partners in the other column, which is the cheapest way to stop edges
  // from criss-crossing. Ties fall back to the semantic group order.
  const destinations = createMemo(() => {
    const raw = [...new Set(props.routes.map((r) => r.destination))];
    const sourceIndex = new Map(
      [...new Set(props.routes.map((r) => r.source))].sort((a, b) => a - b).map((source, i) => [source, i]),
    );
    const bary = (destination: number) => {
      const partners = props.routes.filter((r) => r.destination === destination).map((r) => sourceIndex.get(r.source) ?? 0);
      return partners.reduce((sum, i) => sum + i, 0) / Math.max(1, partners.length);
    };
    return raw.sort((a, b) => bary(a) - bary(b) || DEST_ORDER[DEST_KIND(a)] - DEST_ORDER[DEST_KIND(b)] || a - b);
  });
  const sources = createMemo(() => {
    const raw = [...new Set(props.routes.map((r) => r.source))];
    const destIndex = new Map(destinations().map((destination, i) => [destination, i]));
    const bary = (source: number) => {
      const partners = props.routes.filter((r) => r.source === source).map((r) => destIndex.get(r.destination) ?? 0);
      return partners.reduce((sum, i) => sum + i, 0) / Math.max(1, partners.length);
    };
    return raw.sort((a, b) => bary(a) - bary(b) || SOURCE_ORDER[SOURCE_KIND(a)] - SOURCE_ORDER[SOURCE_KIND(b)] || a - b);
  });
  const rows = () => Math.max(sources().length, destinations().length, 1);
  const height = () => PAD * 2 + rows() * NODE_H + (rows() - 1) * GAP;
  // Centre the shorter column against the taller one.
  const colY = (index: number, count: number) =>
    PAD + ((rows() - count) * (NODE_H + GAP)) / 2 + index * (NODE_H + GAP);
  const srcY = (source: number) => colY(sources().indexOf(source), sources().length);
  const dstY = (destination: number) => colY(destinations().indexOf(destination), destinations().length);
  const routesTo = (destination: number) => props.routes.filter((r) => r.destination === destination);

  return (
    <div class="synth-route-graph">
      <Show
        when={props.routes.length > 0}
        fallback={
          <div class="synth-route-empty">
            <span>{i18n.t("synth.noRoutes")}</span>
            <button onClick={props.onAdd}>{i18n.t("synth.addRoute")}</button>
          </div>
        }
      >
        <svg
          viewBox={`0 0 ${WIDTH} ${height()}`}
          preserveAspectRatio="xMidYMid meet"
          role="group"
          aria-label={i18n.t("synth.routeGraph")}
        >
          <For each={props.routes}>
            {(route) => {
              const x1 = PAD + SRC_W;
              const x2 = DST_X;
              const y1 = () => srcY(route.source) + NODE_H / 2;
              const y2 = () => dstY(route.destination) + NODE_H / 2;
              const magnitude = () => Math.min(1, Math.abs(route.amount));
              const negative = () => route.amount < 0;
              const selected = () => props.selected === route.slot;
              const highlighted = () => props.highlight === route.destination;
              const d = () => {
                const dx = (x2 - x1) * 0.5;
                return `M ${x1} ${y1()} C ${x1 + dx} ${y1()}, ${x2 - dx} ${y2()}, ${x2} ${y2()}`;
              };
              const select = () => props.onSelect(route.slot);
              return (
                <g
                  class="synth-route-edge"
                  classList={{ negative: negative(), selected: selected(), highlighted: highlighted() }}
                  data-slot={route.slot}
                  role="button"
                  tabIndex={0}
                  aria-pressed={selected()}
                  aria-label={`${synthOptionLabel(MOD_SOURCES[route.source])} → ${synthOptionLabel(MOD_DESTINATIONS[route.destination])} ${formatModAmount(route.amount)}`}
                  onClick={select}
                  onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); select(); } }}
                >
                  {/* Wide transparent hit path so thin edges stay clickable. */}
                  <path class="synth-route-hit" d={d()} fill="none" stroke="transparent" stroke-width={10} />
                  <path
                    class="synth-route-line"
                    d={d()}
                    fill="none"
                    stroke={negative() ? "var(--accent-cool)" : "var(--accent)"}
                    stroke-width={selected() ? 1.2 + magnitude() * 3 + 1 : 1 + magnitude() * 3}
                    stroke-opacity={selected() ? 1 : 0.35 + magnitude() * 0.6}
                    stroke-dasharray={negative() ? "4,3" : "none"}
                  />
                </g>
              );
            }}
          </For>

          <For each={sources()}>
            {(source) => (
              <g class="synth-route-node source" data-kind={SOURCE_KIND(source)}>
                <rect x={PAD} y={srcY(source)} width={SRC_W} height={NODE_H} rx={4} />
                <text x={PAD + 8} y={srcY(source) + NODE_H / 2 + 3.5}>{synthOptionLabel(MOD_SOURCES[source])}</text>
              </g>
            )}
          </For>

          <For each={destinations()}>
            {(destination) => {
              const inbound = () => routesTo(destination);
              const selected = () => inbound().some((r) => r.slot === props.selected);
              const select = () => props.onSelect(inbound()[0].slot);
              const total = () => inbound().reduce((sum, r) => sum + r.amount, 0);
              return (
                <g
                  class="synth-route-node destination"
                  classList={{ selected: selected() }}
                  data-kind={DEST_KIND(destination)}
                  data-destination={destination}
                  role="button"
                  tabIndex={0}
                  aria-label={`${synthOptionLabel(MOD_DESTINATIONS[destination])}: ${inbound().length}`}
                  onClick={select}
                  onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); select(); } }}
                >
                  <rect x={DST_X} y={dstY(destination)} width={DST_W} height={NODE_H} rx={4} />
                  <text x={DST_X + 8} y={dstY(destination) + NODE_H / 2 + 3.5}>{synthOptionLabel(MOD_DESTINATIONS[destination])}</text>
                  <text
                    class="synth-route-amount"
                    classList={{ negative: total() < 0 }}
                    x={DST_X + DST_W - 6}
                    y={dstY(destination) + NODE_H / 2 + 3.5}
                    text-anchor="end"
                  >{`${total() > 0 ? "+" : ""}${formatModAmount(total())}`}</text>
                </g>
              );
            }}
          </For>
        </svg>
      </Show>
    </div>
  );
}

export type PatchActionKind = "rename" | "duplicate" | "save-as" | "library";

const PATCH_ACTIONS: readonly { id: PatchActionKind | "share"; labelKey: I18nKey }[] = [
  { id: "rename", labelKey: "synth.ui.renameMenu" },
  { id: "duplicate", labelKey: "synth.ui.duplicate" },
  { id: "save-as", labelKey: "synth.ui.saveAs" },
  { id: "library", labelKey: "synth.ui.saveLibraryMenu" },
  { id: "share", labelKey: "synth.ui.sharePatch" },
];

const PATCH_MENU_GAP = 4;
const PATCH_MENU_MARGIN = 8;
let nextPatchMenuId = 0;

export interface PatchMenuPlacement {
  x: number;
  y: number;
  side: "below" | "above";
}

/** Right-align the menu to its trigger, flipping above when the viewport is short. */
export function placePatchMenu(
  trigger: { right: number; top: number; bottom: number },
  size: { width: number; height: number },
  viewport: { width: number; height: number },
  gap = PATCH_MENU_GAP,
  margin = PATCH_MENU_MARGIN,
): PatchMenuPlacement {
  const below = trigger.bottom + gap;
  const above = trigger.top - gap - size.height;
  const fitsBelow = below + size.height + margin <= viewport.height;
  const side = !fitsBelow && above >= margin ? "above" : "below";
  const preferred = side === "above" ? above : below;
  return {
    x: Math.max(margin, Math.min(trigger.right - size.width, viewport.width - margin - size.width)),
    y: Math.max(margin, Math.min(viewport.height - margin - size.height, preferred)),
    side,
  };
}

export function OpenSynthEditor(props: {
  preset: PresetDef;
  authoredEffects?: Effect[];
  setParam: (name: string, value: string, token?: UndoGestureToken) => Promise<string | null>;
  setParams: (params: [string, string | null][], replaceSynth?: boolean, token?: UndoGestureToken) => Promise<string | null>;
  /** Toggle one macro-learn route; the backend picks the slot and undo step. */
  macroLearn: (
    macroIndex: number,
    control: string,
    token?: UndoGestureToken,
  ) => Promise<{ error: string | null; result: MacroLearnResult | null }>;
  /** Replace the whole patch: parameters, effect chain, and provenance label. */
  loadPatch: (
    name: string,
    patchLabel: string | null,
    params: [string, string | null][],
    effects: Effect[],
    token?: UndoGestureToken,
  ) => Promise<string | null>;
  noteOn: (note: number) => Promise<void>;
  noteOff: (note: number) => Promise<void>;
  /** Audition a candidate; nothing is loaded into the project. */
  auditionPatch: (label: string, params: [string, string | null][], effects?: Effect[]) => Promise<void>;
  stopAudition: () => Promise<void>;
  /** Candidate currently sounding or being prepared, by patch name. */
  auditioning: () => string | null;
  auditionStatus: () => string | null;
  auditionPaused?: () => boolean;
  /** Render two candidates over the same segment; B comes back matched to A. */
  comparePatch?: (a: AbCandidateInput, b: AbCandidateInput) => Promise<AbSnapshot | null>;
  /** Flip between the two measured takes. */
  switchAb?: () => Promise<AbSnapshot | null>;
  /** Last measured pair, used by the browser's switch and readout. */
  abPair?: () => AbSnapshot | null;
  abBusy?: () => boolean;
  abError?: () => string | null;
  clearAbPair?: () => void;
  /** User library rows, refreshed when the browser opens. */
  libraryPatches: () => FactoryPatchInfo[];
  libraryWarnings: () => string[];
  refreshLibrary: () => Promise<string | null>;
  patchActions: {
    rename: (newName: string) => Promise<string | null>;
    duplicate: (newName: string) => Promise<string | null>;
    saveAs: (newName: string) => Promise<string | null>;
    saveToLibrary: (name: string, category: string | null) => Promise<string | null>;
    /** Replace the patch with a safety-bounded variation; the store owns the seed. */
    randomize: (token?: UndoGestureToken) => Promise<string | null>;
  };
}) {
  const [view, setView] = createSignal<View>("sound");
  const [soundPage, setSoundPage] = createSignal<SoundPage>("osc");
  const [modPage, setModPage] = createSignal("matrix");
  const [expanded, setExpanded] = createSignal(true);
  const [drafts, setDrafts] = createSignal<Record<string, number>>({});
  const [browserOpen, setBrowserOpen] = createSignal(false);
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [menuActive, setMenuActive] = createSignal(0);
  const [menuPlacement, setMenuPlacement] = createSignal<PatchMenuPlacement | null>(null);
  const [dialog, setDialog] = createSignal<PatchActionKind | null>(null);
  const patchMenuId = `synth-patch-menu-${++nextPatchMenuId}`;
  let patchField: HTMLButtonElement | undefined;
  let patchMenuTrigger: HTMLButtonElement | undefined;
  let patchMenuElement: HTMLDivElement | undefined;
  const [armedSource, setArmedSource] = createSignal<number | null>(null);
  const [routeError, setRouteError] = createSignal(false);
  const [patchIssue, setPatchIssue] = createSignal<{ key: I18nKey; message: string } | null>(null);
  const setPatchError = (message: string, key: I18nKey = "synth.ui.patchFailure") =>
    setPatchIssue(message ? { key, message } : null);
  /** Row the user chose; it paints before the load resolves. */
  const [chosenPatch, setChosenPatch] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [importing, setImporting] = createSignal(false);
  /** Any whole-patch write is in flight; both affordances wait for it. */
  const [exporting, setExporting] = createSignal(false);
  const busy = () => loading() || importing() || exporting();
  const [activeNotes, setActiveNotes] = createSignal<Set<number>>(new Set());
  const [matrixPage, setMatrixPage] = createSignal(0);
  const [selectedRoute, setSelectedRoute] = createSignal<number | null>(null);
  const [lfoIndex, setLfoIndex] = createSignal(1);
  /** Macro learn: which macro receives the click, and whether clicks are armed. */
  const [learnMacro, setLearnMacro] = createSignal(1);
  const [learnArmed, setLearnArmed] = createSignal(false);
  const [learnMessage, setLearnMessage] = createSignal<LearnMessage | null>(null);
  /** Destination highlighted in the graph while a modulated control is hovered. */
  const [hoveredDestination, setHoveredDestination] = createSignal<number | null>(null);
  let learnBusy = false;
  // The compact strip owns no controls, so collapse cannot leave learn armed.
  createEffect(() => {
    if (!expanded() && learnArmed()) setLearnArmed(false);
  });

  const startNote = (note: number) => {
    if (activeNotes().has(note)) return;
    setActiveNotes((current) => new Set(current).add(note));
    void props.noteOn(note);
  };
  const stopNote = (note: number) => {
    if (!activeNotes().has(note)) return;
    setActiveNotes((current) => {
      const next = new Set(current);
      next.delete(note);
      return next;
    });
    void props.noteOff(note);
  };

  const ownerName = props.preset.name;
  const ownerGeneration = store.documentIdentity().generation;
  let disposed = false;
  onCleanup(() => { disposed = true; });
  const ownsCurrentPreset = () => !disposed && !store.loading()
    && store.documentIdentity().generation === ownerGeneration
    && props.preset.name === ownerName;
  // Both parameter and patch writes use this owner queue. Waiting on begin/end
  // acknowledgments keeps backend undo admission in the same order as edits.
  let mutationQueue: Promise<unknown> = Promise.resolve();
  const mutate = <T,>(action: () => Promise<T>): Promise<T | undefined> => {
    const pending = mutationQueue.then(() => ownsCurrentPreset() ? action() : undefined);
    mutationQueue = pending.catch(() => {});
    return pending;
  };
  const mutateOwned = async (
    action: (token: UndoGestureToken) => Promise<string | null>,
    admitted: () => boolean = () => true,
    owner: ParameterRequestOwner = store.parameterRequestOwner(),
  ): Promise<string | null> => {
    const written = endAutomationWrite();
    try {
      return await mutate(async () => {
        await written;
        if (!ownsCurrentPreset() || !owner() || !admitted()) return null;
        const token = await beginParameterGesture("preset", owner.requireStamp());
        try {
          if (!ownsCurrentPreset() || !owner() || !admitted()) return null;
          return await action(token);
        } finally {
          await endOwnedUndoGesture(token);
        }
      }) ?? null;
    } catch (error) {
      return String(error);
    }
  };
  const setBatch = async (params: [string, string | null][], replaceSynth = false, owner = store.parameterRequestOwner()) => {
    const error = await mutateOwned((token) => props.setParams(params, replaceSynth, token), () => true, owner);
    if (ownsCurrentPreset()) setPatchError(error ?? "");
    return error;
  };
  const draftVersions = new Map<string, number>();
  const touchDraft = (key: string) => {
    const version = (draftVersions.get(key) ?? 0) + 1;
    draftVersions.set(key, version);
    return version;
  };
  const value = (name: string, fallback: number) => drafts()[name]
    ?? storedValue(props.preset, name, specForConcreteKey(name)?.default ?? fallback);
  const releaseAudition = () => {
    for (const note of [...activeNotes()]) stopNote(note);
  };
  window.addEventListener("blur", releaseAudition);
  onCleanup(() => {
    window.removeEventListener("blur", releaseAudition);
    releaseAudition();
    void props.stopAudition();
  });
  const commit = async (name: string, next: number, owner = store.parameterRequestOwner()) => {
    if (busy() || !ownsCurrentPreset()) return;
    const version = touchDraft(name);
    try {
      const error = await mutateOwned((token) => props.setParam(name, String(next), token), () => true, owner);
      if (!ownsCurrentPreset() || draftVersions.get(name) !== version) return;
      setPatchError(error ?? "");
      clearDraft(name);
    } catch (error) {
      if (!ownsCurrentPreset() || draftVersions.get(name) !== version) return;
      setPatchError(String(error));
      clearDraft(name);
    }
  };
  const setDraft = (name: string, next: number) => {
    touchDraft(name);
    setDrafts((current) => ({ ...current, [name]: next }));
  };

  // ── Parameter automation ────────────────────────────────────────────────
  // Every control routes a parameter key through `automationTarget`: the same
  // wrapper yields the lane name, the display<->normalized mapping, and the
  // lane's human label. A write is a gesture owned by `stores/automationWrite`:
  // it samples the transport poll, keeps one point per grid cell, and closes as
  // one undo entry, so this editor only describes the parameter it writes.
  const writeArmed = automationWriteArmed;
  const [affordance, setAffordance] = createSignal<{ key: string; x: number; y: number } | null>(null);

  const automationTrack = () => {
    const selected = store.selectedTrackIndex();
    if (selected !== null && store.project.tracks[selected]?.preset === props.preset.name) return selected;
    return store.project.tracks.findIndex((track) => track.preset === props.preset.name);
  };

  /** Current transport position as an automation point. */
  const automationPosition = (): AutomationPosition => {
    const numerator = Math.max(1, store.project.properties.time_signature.numerator);
    const absolute = Math.max(0, store.transport().position_beats);
    const bar = Math.floor(absolute / numerator) + 1;
    const beatOffset = absolute - (bar - 1) * numerator;
    const beatWhole = Math.floor(beatOffset);
    return {
      bar,
      beat: beatWhole + 1,
      subdivision: Math.min(3, Math.round((beatOffset - beatWhole) * 4)),
    };
  };

  /** Value the control currently shows; drives the one-point affordance. */
  const automationValue = (key: string) => {
    const draft = drafts()[key];
    if (draft !== undefined) return draft;
    return storedValue(props.preset, key, specForConcreteKey(key)?.default ?? 0);
  };

  /** Label of every lane already targeting this preset, for the header. */
  const automatedTargets = createMemo(() => {
    const index = automationTrack();
    if (index < 0) return [] as { key: string; label: string }[];
    const lanes = store.project.tracks[index]?.automation ?? [];
    return lanes
      .map((lane) => automationLaneKey(lane.param))
      .filter((key): key is string => key !== null)
      .map((key) => ({ key, label: automationLabel(key) }));
  });

  const hasAutomationLane = (target: AutomateTarget) => {
    const index = automationTrack();
    if (index < 0) return false;
    return (store.project.tracks[index]?.automation ?? []).some((lane) => lane.param === target.lane);
  };

  /** The lane on this preset's track that drives one patch parameter. */
  const laneForParam = (key: string): { target: string; lane: string } | null => {
    const index = automationTrack();
    if (index < 0) return null;
    const lane = automationLaneName(key);
    const found = (store.project.tracks[index]?.automation ?? []).find((entry) => entry.param === lane);
    return found ? { target: `track:${index}`, lane: found.param } : null;
  };

  /** The read-out for one control; `null` when no lane targets it. */
  const readoutFor = (key: string) => {
    const watched = laneForParam(key);
    return watched ? automationReadout(watched.target, watched.lane) ?? null : null;
  };

  /** A normalized lane value in the control's own unit. */
  const physicalOf = (key: string, normalized: number) => {
    const target = automationTarget(key);
    return target.available ? target.decode(normalized) : normalized;
  };

  const clearDraft = (key: string) => setDrafts((current) => {
    if (!(key in current)) return current;
    const copy = { ...current };
    delete copy[key];
    return copy;
  });

  const encodeAutomationValue = (target: AutomateTarget, physical: number) => {
    const [minimum, maximum] = target.domain;
    if (!Number.isFinite(physical) || physical < minimum || physical > maximum) {
      const spec = specForConcreteKey(target.key);
      const display = (value: number) => spec
        ? formatParameterValue(value, spec.step, spec.unit) : String(value);
      throw new Error(i18n.t("synth.automation.outsideRange", {
        min: display(minimum), max: display(maximum),
      }));
    }
    return target.encode(physical);
  };

  const automatePoint = async (key: string, physical: number) => {
    const target = automationTarget(key);
    const trackIndex = automationTrack();
    if (!target.available || trackIndex < 0) return;
    try {
      await automateOwnedSynthParamAt(
        props.preset.name,
        trackIndex,
        key,
        encodeAutomationValue(target, physical),
        automationPosition(),
        physical,
      );
      setPatchError("");
    } catch (error) {
      setPatchError(String(error));
    }
  };

  const removeAutomation = async (target: AutomateTarget) => {
    const trackIndex = automationTrack();
    if (trackIndex < 0) return;
    try {
      await removeSynthParamAutomation(trackIndex, target.key);
      clearDraft(target.key);
      setPatchError("");
    } catch (error) {
      setPatchError(String(error));
    }
  };

  /** Bar numerator a sampled quarter is read against. */
  const writeNumerator = () => Math.max(1, store.project.properties.time_signature.numerator);

  /** One write surface for a patch parameter; null when it cannot be written. */
  const writeSurface = (key: string, owner = store.parameterRequestOwner()): AutomationWriteSurface | null => {
    const target = automationTarget(key);
    const trackIndex = automationTrack();
    if (!target.available || trackIndex < 0) return null;
    const presetName = props.preset.name;
    const pendingMutations = mutationQueue;
    let token: UndoGestureToken | null = null;
    return {
      key: `synth\u0000${presetName}\u0000${key}`,
      label: key,
      value: () => automationValue(key),
      position: (quarter) => automationPositionAt(quarter, writeNumerator()),
      open: async () => {
        await pendingMutations;
        if (!ownsCurrentPreset() || !owner()) throw new Error(i18n.t("synth.ui.ownerChanged"));
        token = await beginGuardedSynthParamGesture(presetName, trackIndex, key, owner.requireStamp());
      },
      point: async (position, physical) => {
        if (!ownsCurrentPreset() || !owner()) throw new Error(i18n.t("synth.ui.ownerChanged"));
        if (!token) throw new Error(i18n.t("synth.ui.ownerChanged"));
        await updateOwnedSynthParamGesture(token, presetName, key, encodeAutomationValue(target, physical), physical, position);
      },
      close: async () => {
        const closingToken = token;
        token = null;
        if (!closingToken) return;
        const result = await endOwnedSynthParamGesture(closingToken, presetName, key);
        if (result.closed && owner() && ownsCurrentPreset()) store.applyAiSnapshot(result.snapshot);
      },
    };
  };

  const ownedWrites = new Set<AutomationWriteHandle>();
  const rangeGestures = new Set<ReturnType<typeof createParameterGesture>>();
  const rangeGesture = (key: string, current: () => number) => {
    const gesture = createParameterGesture({
      value: current,
      draft: (next) => setDraft(key, next),
      clearDraft: () => clearDraft(key),
      commit: (next) => { void commit(key, next); },
      captureCommit: () => {
        const owner = store.parameterRequestOwner();
        return next => { void commit(key, next, owner); };
      },
      surface: () => ownsCurrentPreset() ? writeSurface(key) : null,
      quarter: () => store.transport().position_beats,
      opened: (handle) => { ownedWrites.add(handle); },
      closed: (handle) => { ownedWrites.delete(handle); },
    });
    rangeGestures.add(gesture);
    onCleanup(() => { gesture.dispose(); rangeGestures.delete(gesture); });
    return gesture;
  };
  const interruptRanges = () => { for (const gesture of rangeGestures) gesture.interrupt(); };
  window.addEventListener("blur", interruptRanges);
  onCleanup(() => {
    window.removeEventListener("blur", interruptRanges);
    for (const handle of ownedWrites) void closeAutomationWrite(handle);
    ownedWrites.clear();
  });
  const focusRange = (event: PointerEvent | MouseEvent) => {
    if (learnArmed() || isSecondaryClick(event)) return;
    if (event.currentTarget instanceof HTMLInputElement) event.currentTarget.focus({ preventScroll: true });
  };
  const resetRange = (event: MouseEvent, gesture: ReturnType<typeof createParameterGesture>, fallback: number) => {
    if (learnArmed() || busy() || isSecondaryClick(event)) return;
    event.preventDefault();
    gesture.reset(fallback);
  };

  const automationKeyFrom = (target: EventTarget | null): string | null => {
    if (!(target instanceof Element)) return null;
    const key = target.closest("[data-param]")?.getAttribute("data-param")?.trim();
    // Patch-effect parameters belong to the FX editor's own controllers.
    if (!key || key.startsWith("fx.")) return null;
    return key;
  };

  /** Value carried by the control that received the event. */
  const automationValueFrom = (event: Event, key: string) => {
    const element = event.target instanceof Element
      ? event.target.closest("[data-automation-value]")
      : null;
    const parsed = element ? Number(element.getAttribute("data-automation-value")) : Number.NaN;
    return Number.isFinite(parsed) ? parsed : automationValue(key);
  };

  const openAutomationMenu = (event: MouseEvent, key: string, physical: number) => {
    const target = automationTarget(key);
    const label = automationLabel(key);
    const trackIndex = automationTrack();
    const items: ContextMenuItem[] = [{ type: "heading", label }];
    if (!target.available) {
      items.push({
        type: "item",
        label: i18n.t("synth.automation.automate"),
        disabled: true,
        hint: target.reason,
        onSelect: () => {},
      });
    } else {
      const missingTrack = trackIndex < 0;
      items.push({
        type: "item",
        label: i18n.t("synth.automation.automate"),
        disabled: missingTrack,
        hint: missingTrack ? i18n.t("synth.automation.noTrack") : undefined,
        onSelect: () => void automatePoint(target.key, physical),
      });
      if (!missingTrack && hasAutomationLane(target)) {
        items.push({
          type: "item",
          label: i18n.t("synth.automation.remove"),
          danger: true,
          onSelect: () => void removeAutomation(target),
        });
      }
    }
    event.preventDefault();
    event.stopPropagation();
    openContextMenu({ x: event.clientX, y: event.clientY, ariaLabel: label, items });
  };

  /** One floating "A" affordance, positioned next to the hovered control. */
  const trackAutomationAffordance = (event: PointerEvent) => {
    if (event.target instanceof Element && event.target.closest(".synth-automate-affordance")) return;
    const key = automationKeyFrom(event.target);
    if (!key) {
      setAffordance(null);
      return;
    }
    const element = (event.target as Element).closest("[data-param]");
    const target = automationTarget(key);
    if (!target.available || !element) {
      setAffordance(null);
      return;
    }
    const rect = element.getBoundingClientRect();
    const next = { key, x: rect.right - 9, y: rect.top + 2 };
    setAffordance((current) => (
      current && current.key === next.key
        && Math.abs(current.x - next.x) < 1
        && Math.abs(current.y - next.y) < 1
        ? current
        : next
    ));
  };

  /** Remember the last parameter the operator touched for "bind last". */
  const rememberTouchedParam = (event: PointerEvent) => {
    if (isSecondaryClick(event)) return;
    // WebKit does not focus range inputs on pointer clicks by default. Keep
    // keyboard edits on the touched control rather than the workspace.
    if (event.target instanceof HTMLInputElement && event.target.type === "range") {
      event.target.focus({ preventScroll: true });
    }
    const key = automationKeyFrom(event.target);
    if (!key) return;
    const index = automationTrack();
    const track = index >= 0 ? store.project.tracks[index] : undefined;
    if (!track) return;
    rememberTweaked({ track: track.name, trackIndex: index, param: key, label: automationLabel(key) });
  };

  const matrixSlots = () => Array.from(
    { length: MATRIX_PAGE_SIZE },
    (_, index) => matrixPage() * MATRIX_PAGE_SIZE + index + 1,
  );
  const usedRoutes = createMemo(() => {
    let used = 0;
    for (let slot = 1; slot <= MATRIX_SLOTS; slot += 1) {
      if (Math.abs(value(`mod.${slot}.amount`, 0)) >= 0.0001) used += 1;
    }
    return used;
  });
  /** Every slot with a non-zero amount, as a graph edge. */
  const activeRoutes = createMemo(() => {
    const routes: { slot: number; source: number; destination: number; amount: number }[] = [];
    for (let slot = 1; slot <= MATRIX_SLOTS; slot += 1) {
      const amount = value(`mod.${slot}.amount`, 0);
      if (Math.abs(amount) < 0.0001) continue;
      const source = Math.round(value(`mod.${slot}.source`, 0));
      const destination = Math.round(value(`mod.${slot}.destination`, 0));
      if (source < 0 || source >= MOD_SOURCES.length || destination < 0 || destination >= MOD_DESTINATIONS.length) continue;
      routes.push({ slot, source, destination, amount });
    }
    return routes;
  });
  const removeRoute = async (slot: number) => {
    await commit(`mod.${slot}.amount`, 0);
    if (selectedRoute() === slot) setSelectedRoute(null);
  };

  // ── Macro learn and reverse lookup ──────────────────────────────────────
  const destinationLabel = (destination: number) =>
    synthOptionLabel(MOD_DESTINATIONS[destination] ?? String(destination));
  /** Controls the catalog can learn, with the destination each one drives. */
  const controlDestination = (key: string) => MOD_DESTINATION_BY_CONTROL.get(key);
  const inboundRoutes = (destination: number) =>
    activeRoutes().filter((route) => route.destination === destination);
  const modulationTooltip = (key: string) => {
    const destination = controlDestination(key);
    if (destination === undefined) return null;
    const routes = inboundRoutes(destination);
    if (routes.length === 0) return null;
    return {
      title: i18n.t("synth.modulated.title"),
      subtitle: destinationLabel(destination),
      rows: routes.map((route) => ({
        label: synthOptionLabel(MOD_SOURCES[route.source] ?? String(route.source)),
        value: `${route.amount > 0 ? "+" : ""}${formatModAmount(route.amount)}`,
        mono: true,
      })),
    };
  };
  // One attachment per learnable control, created once so a re-render cannot
  // strand a tooltip.
  const modulationAttachments = new Map<string, TooltipAttachment>();
  for (const target of SYNTH_CATALOG.modLearnTargets) {
    modulationAttachments.set(target.control, useTooltip(() => modulationTooltip(target.control)));
  }
  /** Tooltip + hover highlight for a control with an inbound route. */
  const modulationProps = (key: string) => {
    const destination = controlDestination(key);
    const attachment = modulationAttachments.get(key);
    if (destination === undefined || !attachment) return null;
    const active = () => inboundRoutes(destination).length > 0;
    return {
      active,
      attrs: {
        ...attachment,
        onMouseEnter: (event: MouseEvent & { currentTarget: Element }) => {
          attachment.onMouseEnter(event);
          setHoveredDestination(destination);
        },
        onMouseLeave: () => {
          attachment.onMouseLeave();
          setHoveredDestination(null);
        },
        onFocusIn: (event: FocusEvent & { currentTarget: Element }) => {
          attachment.onFocusIn(event);
          setHoveredDestination(destination);
        },
        onFocusOut: () => {
          attachment.onFocusOut();
          setHoveredDestination(null);
        },
      } satisfies TooltipAttachment,
    };
  };
  /** The control key under a learn click, ignoring the learn bar itself. */
  const learnControlKey = (target: EventTarget | null) => {
    if (!(target instanceof Element) || target.closest("[data-learn-ui]")) return null;
    const key = target.closest("[data-param]")?.getAttribute("data-param")?.trim();
    return key ? key : null;
  };
  const learnControl = async (key: string) => {
    // A whole-patch load owns the preset; a learn would race its snapshot.
    if (learnBusy || busy()) return;
    const owner = store.parameterRequestOwner();
    learnBusy = true;
    try {
      const macro = learnMacro();
      const written = endAutomationWrite();
      const learned = await mutate(async () => {
        await written;
        if (!ownsCurrentPreset() || !owner()) return undefined;
        const token = await beginParameterGesture("preset", owner.requireStamp());
        try {
          if (!ownsCurrentPreset() || !owner()) return undefined;
          return await props.macroLearn(macro, key, token);
        } finally {
          await endOwnedUndoGesture(token);
        }
      });
      if (!ownsCurrentPreset() || !learned) return;
      const { error, result } = learned;
      if (!result) {
        setLearnMessage({
          key: "synth.learn.failed",
          values: { message: error ?? "" },
        });
        return;
      }
      if (result.status === "unsupported") {
        setLearnMessage({
          key: "synth.learn.unsupported",
          values: { control: `${paramLabel(key)} (${key})` },
        });
        return;
      }
      if (result.status === "full") {
        setLearnMessage({ key: "synth.learn.full" });
        return;
      }
      const slot = result.slot;
      setLearnMessage({
        key: result.status === "bound" ? "synth.learn.bound" : "synth.learn.unbound",
        values: {
          macro,
          destination:
            result.destination === null ? "" : destinationLabel(result.destination),
          slot: slot === null ? "" : String(slot).padStart(2, "0"),
        },
      });
      setPatchError("");
      if (slot !== null) {
        setMatrixPage(Math.floor((slot - 1) / MATRIX_PAGE_SIZE));
        setSelectedRoute(slot);
      }
    } catch (error) {
      if (ownsCurrentPreset()) setLearnMessage({ key: "synth.learn.failed", values: { message: String(error) } });
    } finally {
      learnBusy = false;
    }
  };
  const focusLearnControl = (target: EventTarget | null) => {
    if (!(target instanceof Element)) return;
    const row = target.closest("[data-param]");
    const control = target.closest<HTMLElement>("input,button,select,[tabindex]")
      ?? row?.querySelector<HTMLElement>("input,button,select,[tabindex]");
    control?.focus({ preventScroll: true });
  };
  const learnPointerDown = (event: PointerEvent) => {
    if (!learnArmed() || isSecondaryClick(event)) return;
    const key = learnControlKey(event.target);
    if (!key) return;
    event.preventDefault();
    event.stopPropagation();
    focusLearnControl(event.target);
    void learnControl(key);
  };
  const learnClick = (event: MouseEvent) => {
    if (!learnArmed() || isSecondaryClick(event)) return;
    const key = learnControlKey(event.target);
    if (!key) return;
    event.preventDefault();
    event.stopPropagation();
    // Learning prevents the native input action, but focus still belongs to
    // this control so Escape exits learning before workspace shortcuts run.
    focusLearnControl(event.target);
    // A pointer learn already ran on pointerdown; only keyboard/screen-reader
    // activation (detail 0) still needs this path.
    if (event.detail === 0) void learnControl(key);
  };
  const learnKeyDown = (event: KeyboardEvent) => {
    if (!learnArmed()) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      setLearnArmed(false);
      return;
    }
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    const key = learnControlKey(event.target);
    if (key && ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) {
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    if (event.key !== "Enter" && event.key !== " ") return;
    if (!key) return;
    event.preventDefault();
    event.stopPropagation();
    if (!event.repeat) void learnControl(key);
  };
  // Capture phase runs before a slider or dropdown can start its own edit.
  let workbenchElement: HTMLElement | undefined;
  const attachLearnCapture = (element: HTMLElement) => {
    if (workbenchElement) {
      workbenchElement.removeEventListener("pointerdown", learnPointerDown, true);
      workbenchElement.removeEventListener("click", learnClick, true);
      workbenchElement.removeEventListener("keydown", learnKeyDown, true);
    }
    workbenchElement = element;
    element.addEventListener("pointerdown", learnPointerDown, true);
    element.addEventListener("click", learnClick, true);
    element.addEventListener("keydown", learnKeyDown, true);
  };
  onCleanup(() => {
    if (!workbenchElement) return;
    workbenchElement.removeEventListener("pointerdown", learnPointerDown, true);
    workbenchElement.removeEventListener("click", learnClick, true);
    workbenchElement.removeEventListener("keydown", learnKeyDown, true);
  });
  const learnStatusText = () => {
    if (learnMessage()) {
      const message = learnMessage()!;
      return i18n.t(message.key, message.values);
    }
    return learnArmed()
      ? i18n.t("synth.learn.armed", { macro: learnMacro() })
      : i18n.t("synth.learn.idle");
  };

  const libraryParams = (info: FactoryPatchInfo): [string, string | null][] =>
    (info.params ?? []).map(([key, value]) => [key, paramText(value)] as [string, string | null]);
  const patchRows = () => factoryPatchInfos(props.libraryPatches());
  const findPatch = (name: string) => patchRows().find((patch) => patch.name === name);
  /** Writes are ordered; the newest queued choice supersedes unstarted loads.
   * An already running load may finish but cannot dismiss the newer choice. */
  let loadSeq = 0;
  const loadPatch = async (info: FactoryPatchInfo, owner = store.parameterRequestOwner()) => {
    // Repeating the patch already loading would only duplicate the work.
    if (loading() && chosenPatch() === info.name) return;
    const seq = ++loadSeq;
    // The row paints as chosen now; the project catches up when the load lands.
    setChosenPatch(info.name);
    setLoading(true);
    setPatchError("");
    // A load ends any running candidate audition before the project changes.
    void props.stopAudition();
    try {
      const params = info.tier === "user" ? libraryParams(info) : factoryParams(info.name);
      const error = await mutateOwned(
        (token) => props.loadPatch(ownerName, info.name, params, info.effects ?? [], token),
        () => seq === loadSeq,
        owner,
      );
      if (!ownsCurrentPreset() || seq !== loadSeq) return;
      if (error) {
        setChosenPatch(null);
        setPatchError(error);
        return;
      }
      setBrowserOpen(false);
      releaseAudition();
      setDrafts({});
    } finally {
      if (seq === loadSeq) setLoading(false);
    }
  };
  const auditionPatch = (name: string) => {
    if (busy()) return;
    const info = findPatch(name);
    if (info?.tier === "user") {
      void props.auditionPatch(name, libraryParams(info), info.effects ?? []);
    } else {
      void props.auditionPatch(name, factoryParams(name), []);
    }
  };
  /** A is the patch loaded in this editor; B is the candidate row. */
  const comparePatch = (name: string) => {
    if (busy() || props.abBusy?.()) return;
    const info = findPatch(name);
    if (!info) return;
    const a: AbCandidateInput = {
      label: props.preset.name,
      params: props.preset.params.map(([key, value]) => [key, paramText(value)]),
      effects: props.preset.effects ?? [],
    };
    const b: AbCandidateInput = {
      label: name,
      params: info.tier === "user" ? libraryParams(info) : factoryParams(name),
      effects: info.tier === "user" ? info.effects ?? [] : [],
    };
    void props.comparePatch?.(a, b);
  };
  const openBrowser = () => {
    setMenuOpen(false);
    setChosenPatch(null);
    void props.refreshLibrary();
    setBrowserOpen(true);
  };
  const closeBrowser = () => {
    setBrowserOpen(false);
    props.clearAbPair?.();
    void props.stopAudition();
  };
  /** Category recorded with a library save: the factory origin's own category. */
  const provenanceCategory = () => {
    const origin = props.preset.patch;
    if (!origin) return null;
    return FACTORY_PATCH_INFO.get(origin)?.category
      ?? props.libraryPatches().find((patch) => patch.name === origin)?.category
      ?? null;
  };
  const openDialog = (kind: PatchActionKind) => {
    setMenuOpen(false);
    setDialog(kind);
  };
  const randomizePatch = async () => {
    if (busy()) return;
    setLoading(true);
    setPatchError("");
    try {
      const error = await mutateOwned((token) => props.patchActions.randomize(token));
      if (!ownsCurrentPreset()) return;
      if (error) {
        setPatchError(error);
        return;
      }
      setDrafts({});
      releaseAudition();
    } finally {
      setLoading(false);
    }
  };

  const closeMenu = (restoreFocus = false) => {
    if (!menuOpen()) return;
    setMenuOpen(false);
    setMenuPlacement(null);
    if (restoreFocus) patchMenuTrigger?.focus({ preventScroll: true });
  };
  const openMenu = () => {
    setMenuActive(0);
    setMenuPlacement(null);
    setMenuOpen(true);
  };
  const runMenuAction = (index: number) => {
    const action = PATCH_ACTIONS[index];
    if (!action) return;
    setMenuOpen(false);
    setMenuPlacement(null);
    if (action.id === "share") {
      patchMenuTrigger?.focus({ preventScroll: true });
      void sharePatch();
      return;
    }
    openDialog(action.id);
  };
  const moveMenuActive = (delta: number) => {
    const total = PATCH_ACTIONS.length;
    setMenuActive((current) => (current + delta + total) % total);
  };
  createEffect(() => {
    if (!menuOpen()) return;
    const trigger = patchMenuTrigger;
    if (!trigger) return;

    const reposition = () => {
      if (!patchMenuElement) return;
      const anchor = trigger.getBoundingClientRect();
      // Clear the header row, not just the button's own box: its padding is
      // still header, and the menu must not paint over the border.
      const row = trigger.closest(".synth-patch-browser")?.getBoundingClientRect().bottom;
      const size = patchMenuElement.getBoundingClientRect();
      setMenuPlacement(placePatchMenu(
        { right: anchor.right, top: anchor.top, bottom: Math.max(anchor.bottom, row ?? anchor.bottom) },
        size,
        { width: window.innerWidth || 1024, height: window.innerHeight || 768 },
      ));
    };

    // The portalled menu mounts before this effect; measure once it is styled.
    queueMicrotask(() => {
      if (!menuOpen()) return;
      reposition();
      patchMenuElement?.focus({ preventScroll: true });
    });

    const onPointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (trigger.contains(target) || patchMenuElement?.contains(target)) return;
      closeMenu();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        closeMenu(true);
      } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        event.stopPropagation();
        moveMenuActive(event.key === "ArrowDown" ? 1 : -1);
      } else if (event.key === "Home" || event.key === "End") {
        event.preventDefault();
        event.stopPropagation();
        setMenuActive(event.key === "Home" ? 0 : PATCH_ACTIONS.length - 1);
      } else if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        event.stopPropagation();
        runMenuAction(menuActive());
      } else if (event.key === "Tab") {
        closeMenu();
      } else {
        // Keep app shortcuts from firing behind the open menu.
        event.stopPropagation();
      }
    };
    const onScroll = () => reposition();
    const onResize = () => reposition();
    const onWindowBlur = () => closeMenu();

    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onResize);
    window.addEventListener("blur", onWindowBlur);
    onCleanup(() => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onResize);
      window.removeEventListener("blur", onWindowBlur);
    });
  });
  const dialogTitle = () => {
    switch (dialog()) {
      case "rename": return i18n.t("synth.ui.renamePatch");
      case "duplicate": return i18n.t("synth.ui.duplicatePatch");
      case "save-as": return i18n.t("synth.ui.savePatchAs");
      default: return i18n.t("synth.ui.saveLibrary");
    }
  };
  const dialogInitial = () => {
    switch (dialog()) {
      case "rename": return props.preset.name;
      case "duplicate": return `${props.preset.name} copy`;
      case "save-as": return `${props.preset.name} 2`;
      default: return props.preset.name;
    }
  };
  const dialogConfirmLabel = () => (i18n.t(dialog() === "rename" ? "synth.ui.rename" : dialog() === "library" ? "synth.ui.save" : "synth.ui.create"));
  const confirmDialog = async (newName: string): Promise<string | null> => {
    switch (dialog()) {
      case "rename": return props.patchActions.rename(newName);
      case "duplicate": return props.patchActions.duplicate(newName);
      case "save-as": return props.patchActions.saveAs(newName);
      default: return props.patchActions.saveToLibrary(newName, provenanceCategory());
    }
  };
  const browseWavetable = async () => {
    const owner = store.parameterRequestOwner();
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({
        multiple: false,
        filters: [{ name: i18n.t("synth.ui.wavetableWav"), extensions: ["wav"] }],
      });
      if (!path || Array.isArray(path)) return;
      const error = await setBatch([["wavetable.file", path], ["osc.a.waveform", "4"]], false, owner);
      setPatchError(error ?? "", "synth.ui.wavetableFailure");
    } catch (error) {
      setPatchError(String(error), "synth.ui.wavetableFailure");
    }
  };
  const sharePatch = async () => {
    if (busy()) return;
    const presetName = props.preset.name;
    const category = provenanceCategory();
    const generation = store.documentIdentity().generation;
    setExporting(true);
    setPatchError("");
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const name = presetName.replace(/[<>:"/\\|?*]/g, "_");
      const path = await save({
        title: i18n.t("synth.ui.sharePatch"),
        defaultPath: `${name}.ocppatch`,
        filters: [{ name: i18n.t("synth.ui.patchBundle"), extensions: ["ocppatch"] }],
      });
      if (!path || store.documentIdentity().generation !== generation) return;
      await exportPatchBundle(
        presetName,
        category,
        /\.ocppatch$/i.test(path) ? path : `${path}.ocppatch`,
      );
    } catch (error) {
      setPatchError(String(error), "synth.ui.shareFailure");
    } finally {
      setExporting(false);
    }
  };

  const importPresetAt = async (path: string, owner = store.parameterRequestOwner()) => {
    if (busy() || !owner()) return;
    const presetName = props.preset.name;
    const generation = store.documentIdentity().generation;
    setImporting(true);
    setPatchError("");
    try {
      if (/\.ocppatch$/i.test(path)) {
        const imported = await importPatchBundle(path);
        await props.refreshLibrary();
        if (ownsCurrentPreset() && owner() && store.documentIdentity().generation === generation && props.preset.name === presetName) {
          await loadPatch({ ...imported, tier: "user" }, owner);
        }
        return;
      }
      const imported = await importExternalPreset(path);
      if (!ownsCurrentPreset() || !owner() || store.documentIdentity().generation !== generation || props.preset.name !== presetName) return;
      log.info(
        "synth",
        `Imported ${imported.name || imported.label}`
          + `${imported.author ? ` by ${imported.author}` : ""} from ${imported.label}`,
      );
      if (imported.warnings.length > 0) {
        log.warn("synth", `${imported.label}: ${imported.warnings.join("; ")}`);
      }
      // The same whole-patch load the browser uses: parameters and the effect
      // chain are replaced, and the file name becomes the provenance label.
      void props.stopAudition();
      const error = await mutateOwned((token) => props.loadPatch(presetName, imported.label, imported.params, [], token), () => true, owner);
      if (!ownsCurrentPreset()) return;
      if (error) {
        setPatchError(error, "synth.ui.importFailure");
        return;
      }
      setBrowserOpen(false);
      releaseAudition();
      setDrafts({});
    } catch (error) {
      setPatchError(String(error), "synth.ui.importFailure");
    } finally {
      setImporting(false);
    }
  };
  /** LOAD: pick one external file, then import it as a whole patch. */
  const importPreset = async () => {
    if (busy()) return;
    const owner = store.parameterRequestOwner();
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({
        multiple: false,
        title: i18n.t("synth.importPresetTitle"),
        defaultPath: rememberedPresetDir(),
        filters: [{ name: i18n.t("synth.ui.presetFiles"), extensions: ["ocppatch", "SerumPreset"] }],
      });
      if (!path || Array.isArray(path)) return;
      rememberPresetDir(path);
      await importPresetAt(path, owner);
    } catch (error) {
      setPatchError(String(error), "synth.ui.importFailure");
    }
  };
  const assignRoute = async (source: number, destination: number) => {
    if (!Number.isInteger(source) || source < 0 || source >= MOD_SOURCES.length) return;
    let slot: number | null = null;
    for (let candidate = 1; candidate <= MATRIX_SLOTS; candidate += 1) {
      if (Math.abs(value(`mod.${candidate}.amount`, 0)) < 0.0001) {
        slot = candidate;
        break;
      }
    }
    if (slot === null) {
      setRouteError(true);
      return;
    }
    setRouteError(false);
    const error = await setBatch([
      [`mod.${slot}.source`, String(source)],
      [`mod.${slot}.destination`, String(destination)],
      [`mod.${slot}.amount`, "0.5"],
    ]);
    if (error) {
      setPatchError(error);
      return;
    }
    setPatchError("");
    // Follow the new route to its page so 17–64 stay discoverable.
    setMatrixPage(Math.floor((slot - 1) / MATRIX_PAGE_SIZE));
    setArmedSource(null);
    setSelectedRoute(slot);
  };

  const Range = (control: {
    name: string; label: string;
    /** Explanatory tooltip for controls whose label cannot carry the meaning. */
    hint?: () => string;
  }) => {
    const spec = specForConcreteKey(control.name);
    if (!spec) throw new Error(`Missing synth parameter schema: ${control.name}`);
    const { min, max, step, default: fallback } = spec;
    const unit = spec.unit;
    const position = () => parameterPosition(spec, current());
    const fromInput = (raw: string) => parameterFromPosition(spec, Number(raw));
    const id = `open-synth-${control.name.replaceAll(".", "-")}`;
    const modulation = modulationProps(control.name);
    // A lane drives the value and shows a badge until a drag takes over.
    const lane = createMemo(() => laneForParam(control.name));
    createEffect(() => {
      const watched = lane();
      if (!watched) return;
      watchAutomation(watched.target, watched.lane, untrack(() => store.transport().position_beats));
      onCleanup(() => unwatchAutomation(watched.target, watched.lane));
    });
    const answer = () => readoutFor(control.name);
    const driving = () => {
      const live = answer();
      return live?.status === "driven" ? live : null;
    };
    const driven = () => (drafts()[control.name] === undefined ? driving() : null);
    const driveHint = () => {
      if (!lane() || drafts()[control.name] !== undefined) return null;
      const current = answer();
      return current?.status === "not_driven" ? current.reason : null;
    };
    const tooltip = useTooltip(() => driveHint() ?? (control.hint ? control.hint() : null));
    const current = () => {
      const draft = drafts()[control.name];
      if (draft !== undefined) return draft;
      const live = driven();
      if (live) return live.domain === "normalized" ? physicalOf(control.name, live.value) : live.value;
      return value(control.name, fallback);
    };
    const gesture = rangeGesture(control.name, current);
    return (
      <div
        class="synth-control"
        classList={{
          "synth-modulated": !!modulation && modulation.active(),
          driven: driven() !== null,
        }}
        data-param={control.name}
        data-param-min={min}
        data-param-max={max}
        data-param-step={step}
        data-range-input={id}
        data-automation-value={String(current())}
        data-driven={driven() !== null ? "true" : undefined}
        {...tooltip}
        onMouseEnter={(event) => {
          if (modulation?.active()) modulation.attrs.onMouseEnter(event);
          else tooltip.onMouseEnter(event);
          setHoveredDestination(modulation?.active() ? controlDestination(control.name) ?? null : null);
        }}
        onMouseLeave={() => {
          tooltip.onMouseLeave();
          modulation?.attrs.onMouseLeave();
          setHoveredDestination(null);
        }}
        onFocusIn={(event) => {
          tooltip.onFocusIn(event);
          setHoveredDestination(modulation?.active() ? controlDestination(control.name) ?? null : null);
        }}
        onFocusOut={() => {
          tooltip.onFocusOut();
          modulation?.attrs.onFocusOut();
          setHoveredDestination(null);
        }}
      >
        <SynthParameterButton
          id={`${id}-button`}
          label={control.label}
          valueText={formatParameterValue(current(), step, unit)}
          position={position()}
          disabled={busy()}
          editable={!learnArmed()}
          onDragStart={(event) => gesture.pointerDown(event)}
          onDragPosition={(next) => gesture.input(parameterFromPosition(spec, next))}
          onDragCancel={(event) => gesture.pointerCancel(event)}
          onStep={(key) => {
            const next = parameterKeyValue(spec, current(), key);
            if (next !== null) gesture.discrete(next);
          }}
          onReset={(event) => resetRange(event, gesture, fallback)}
          onClose={() => gesture.interrupt()}
          decoration={<>
            <Show when={modulation?.active()}>
              <span class="synth-modulated-dot" aria-hidden="true" />
            </Show>
            <Show when={driven()}>
              <span
                class="synth-driven-badge"
                data-driven-badge="true"
                aria-label={i18n.t("automation.drivenBadge")}
                title={i18n.t("automation.drivenTitle")}
              >A</span>
            </Show>
          </>}
        >
        <input
          id={id}
          type="range"
          aria-label={control.label}
          data-param={control.name}
          data-automation-value={String(current())}
          aria-valuetext={formatParameterValue(current(), step, unit)}
          min="0"
          max="1"
          step="any"
          value={position()}
          style={{ "--slider-fill": `${Math.round(position() * 1000) / 10}%` }}
          disabled={busy()}
          onPointerUp={focusRange}
          onClick={focusRange}
          onBlur={(event) => {
            const next = event.relatedTarget;
            if (next === null || next === document.body || next === document.documentElement) return;
            gesture.interrupt();
          }}
          onPointerDown={(event) => { if (!busy() && !learnArmed()) gesture.pointerDown(event); }}
          onInput={(event) => {
            if (busy() || learnArmed()) return;
            gesture.input(fromInput(event.currentTarget.value));
          }}
          onChange={(event) => {
            if (busy() || learnArmed()) return;
            gesture.change(fromInput(event.currentTarget.value));
          }}
          onKeyDown={(event) => {
            if (busy() || event.metaKey || event.ctrlKey || event.altKey) return;
            const next = parameterKeyValue(spec, current(), event.key);
            if (next === null) return;
            event.preventDefault();
            gesture.discrete(next);
          }}
          onPointerCancel={(event) => gesture.pointerCancel(event)}
          onDblClick={(event) => resetRange(event, gesture, fallback)}
        />
        </SynthParameterButton>
      </div>
    );
  };

  /** Discrete controls write one lane point per choice. */
  const commitDiscrete = async (name: string, next: number, owner: ParameterRequestOwner) => {
    if (!ownsCurrentPreset() || !owner()) return;
    if (writeArmed()) {
      const surface = writeSurface(name, owner);
      if (surface) {
        const handle = writeAutomationOnce(surface, next, store.transport().position_beats);
        if (handle) await closeAutomationWrite(handle);
        return;
      }
    }
    await commit(name, next, owner);
  };
  const Select = (control: {
    name: string; label: string; options: string[]; fallback: number; compact?: boolean;
    hint?: () => string;
  }) => {
    let menuOwner: ParameterRequestOwner | null = null;
    const modulation = modulationProps(control.name);
    const hintTooltip = control.hint ? useTooltip(control.hint) : null;
    return (
      <Dropdown
        class="synth-select-control"
        dataParam={control.name}
        disabled={busy()}
        label={control.label}
        stack={control.compact}
        value={Math.round(value(control.name, control.fallback))}
        options={control.options.map((label, index) => ({ value: index, label: synthOptionLabel(label) }))}
        triggerAttrs={{ "data-automation-value": String(Math.round(value(control.name, control.fallback))) }}
        triggerProps={{ ...(hintTooltip ?? {}), ...(modulation?.attrs ?? {}) }}
        onOpen={() => { menuOwner = store.parameterRequestOwner(); }}
        onClose={() => { menuOwner = null; }}
        onSelect={(next) => {
          const owner = menuOwner;
          if (owner) void commitDiscrete(control.name, next, owner);
        }}
      />
    );
  };
  const ValueSelect = (control: {
    name: string; label: string; options: [number, string][]; fallback: number; compact?: boolean;
    hint?: () => string;
  }) => {
    let menuOwner: ParameterRequestOwner | null = null;
    const modulation = modulationProps(control.name);
    const hintTooltip = control.hint ? useTooltip(control.hint) : null;
    return (
      <Dropdown
        class="synth-select-control"
        dataParam={control.name}
        disabled={busy()}
        label={control.label}
        stack={control.compact}
        value={Math.round(value(control.name, control.fallback))}
        options={control.options.map(([optionValue, label]) => ({ value: optionValue, label: synthOptionLabel(label) }))}
        triggerAttrs={{ "data-automation-value": String(Math.round(value(control.name, control.fallback))) }}
        triggerProps={{ ...(hintTooltip ?? {}), ...(modulation?.attrs ?? {}) }}
        onOpen={() => { menuOwner = store.parameterRequestOwner(); }}
        onClose={() => { menuOwner = null; }}
        onSelect={(next) => {
          const owner = menuOwner;
          if (owner) void commitDiscrete(control.name, next, owner);
        }}
      />
    );
  };
  const Toggle = (control: {
    name: string;
    label: string;
    fallback: number;
    /** A disabled control must say why; the hint shows on hover and focus. */
    disabled?: () => boolean;
    hint?: () => string | null;
  }) => {
    let interactionOwner: ParameterRequestOwner | null = null;
    let canceled = false;
    let interactionVersion = 0;
    const capture = () => { interactionVersion += 1; interactionOwner = store.parameterRequestOwner(); canceled = false; };
    const disabled = () => busy() || (control.disabled?.() ?? false);
    const hintTooltip = control.hint ? useTooltip(control.hint) : null;
    const modulation = modulationProps(control.name);
    return (
      <label
        class="synth-toggle-control"
        classList={{
          "synth-modulated": !!modulation && modulation.active(),
          "is-disabled": disabled(),
        }}
        data-param={control.name}
        data-automation-value={String(value(control.name, control.fallback) >= 0.5 ? 1 : 0)}
        tabIndex={disabled() ? 0 : undefined}
        title={disabled() ? control.hint?.() ?? undefined : undefined}
        {...(hintTooltip ?? {})}
        {...(modulation?.attrs ?? {})}
      >
        <input
          type="checkbox"
          aria-label={control.label}
          checked={value(control.name, control.fallback) >= 0.5}
          disabled={disabled()}
          onPointerDown={(event) => { if ((event.button ?? 0) === 0 && !isSecondaryClick(event)) capture(); }}
          onKeyDown={(event) => { if (!event.repeat && (event.key === " " || event.key === "Enter")) capture(); }}
          onInput={() => { if (!interactionOwner || canceled) capture(); }}
          onPointerCancel={() => { canceled = true; }}
          onBlur={() => { if (interactionOwner) canceled = true; }}
          onChange={(event) => {
            const input = event.currentTarget;
            const next = input.checked ? 1 : 0;
            const owner = interactionOwner ?? store.parameterRequestOwner();
            interactionOwner = null;
            const version = ++interactionVersion;
            const restore = () => {
              if (version === interactionVersion) input.checked = value(control.name, control.fallback) >= 0.5;
            };
            if (canceled || !owner() || !ownsCurrentPreset()) { canceled = false; restore(); return; }
            void commitDiscrete(control.name, next, owner).finally(restore);
          }}
        />
        <span>
          <Show when={modulation?.active()}>
            <span class="synth-modulated-dot" aria-hidden="true" />
          </Show>
          {control.label}
          <Show when={disabled()}>
            <span class="synth-toggle-reason" aria-hidden="true">?</span>
          </Show>
        </span>
      </label>
    );
  };

  /** Catalog-driven slider: one loop covers every new control. */
  const CatalogRange = (key: string) => {
    const spec = specForConcreteKey(key);
    if (!spec || spec.control !== "slider") return null;
    return Range({ name: key, label: paramLabel(key).toUpperCase() });
  };
  /** Catalog-driven enum select, including the no-op status suffix. */
  const CatalogSelect = (key: string) => {
    const spec = specForConcreteKey(key);
    if (!spec || spec.control !== "select" || !spec.enumId) return null;
    return Select({
      name: key,
      label: paramLabel(key).toUpperCase(),
      options: enumOptions(spec.enumId, true).map((option) => option.label),
      fallback: spec.default,
      compact: true,
    });
  };

  const amp = () => ({
    attack: value("amp.attack", 0.008), decay: value("amp.decay", 0.16),
    sustain: value("amp.sustain", 0.72), release: value("amp.release", 0.28), curve: value("amp.curve", 0),
  });
  const filterEnv = () => ({
    attack: value("filter_env.attack", 0.01), decay: value("filter_env.decay", 0.2),
    sustain: value("filter_env.sustain", 0), release: value("filter_env.release", 0.25), curve: value("filter_env.curve", 0),
  });

  return (
    <section
      ref={attachLearnCapture}
      class="synth-workbench"
      classList={{
        expanded: expanded(),
        "synth-write-armed": writeArmed(),
        "synth-learn-armed": learnArmed(),
      }}
      aria-label={i18n.t("synth.ui.controls")}
      onContextMenu={(event) => {
        const key = automationKeyFrom(event.target);
        if (!key) return;
        openAutomationMenu(event, key, automationValueFrom(event, key));
      }}
      onPointerDown={rememberTouchedParam}
      onPointerMove={trackAutomationAffordance}
      onPointerLeave={() => setAffordance(null)}
    >
      <header class="synth-workbench-header">
        <div class="synth-brand-block">
          <span class="synth-brand-mark">OS</span>
          <strong>OpenSynth</strong>
        </div>
        <div class="synth-patch-browser">
          <button
            ref={(el) => { patchField = el; }}
            class="synth-patch-field"
            data-patch-trigger
            aria-label={i18n.t("synth.patchField")}
            aria-haspopup="dialog"
            aria-expanded={browserOpen()}
            disabled={busy()}
            onClick={() => (browserOpen() ? closeBrowser() : openBrowser())}
            {...useTooltip(() => i18n.t("synth.patchFieldTooltip"))}
          >
            <span class="synth-patch-name">{props.preset.name}</span>
            <Show when={props.preset.patch}>
              {(origin) => <span class="synth-patch-origin">{origin()}</span>}
            </Show>
            <span class="synth-patch-caret" aria-hidden="true">▾</span>
          </button>
          <button
            class="synth-load-button"
            aria-label={i18n.t("synth.loadExternal")}
            disabled={busy()}
            onClick={() => void importPreset()}
            {...useTooltip(() => i18n.t("synth.loadExternal"))}
          >
            {i18n.t(importing() ? "synth.ui.loading" : "synth.ui.load").toUpperCase()}
          </button>
          <button
            class="synth-randomize-button"
            aria-label={i18n.t("synth.ui.randomize")}
            disabled={busy()}
            onClick={() => void randomizePatch()}
            {...useTooltip(() => i18n.t("synth.ui.randomizeHint"))}
          >
            <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"
              fill="none" stroke="currentColor" stroke-width="1.2">
              <rect x="2.4" y="2.4" width="11.2" height="11.2" rx="2.2" />
              <circle cx="5.7" cy="5.7" r="0.85" fill="currentColor" stroke="none" />
              <circle cx="10.3" cy="5.7" r="0.85" fill="currentColor" stroke="none" />
              <circle cx="8" cy="8" r="0.85" fill="currentColor" stroke="none" />
              <circle cx="5.7" cy="10.3" r="0.85" fill="currentColor" stroke="none" />
              <circle cx="10.3" cy="10.3" r="0.85" fill="currentColor" stroke="none" />
            </svg>
          </button>
          <div class="synth-patch-actions">
            <button
              ref={(el) => { patchMenuTrigger = el; }}
              class="synth-patch-menu-button"
              aria-label={i18n.t("synth.ui.actions")}
              aria-haspopup="menu"
              aria-expanded={menuOpen()}
              aria-controls={menuOpen() ? patchMenuId : undefined}
              onClick={() => (menuOpen() ? closeMenu() : openMenu())}
              onKeyDown={(event) => {
                if (menuOpen()) return;
                if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  event.stopPropagation();
                  openMenu();
                }
              }}
            >
              ⋯
            </button>
          </div>
          <Show when={menuOpen()}>
            <Portal>
              <div
                ref={(el) => { patchMenuElement = el; }}
                id={patchMenuId}
                class="synth-patch-menu"
                role="menu"
                aria-label={i18n.t("synth.ui.actions")}
                aria-activedescendant={`${patchMenuId}-item-${menuActive()}`}
                tabIndex={-1}
                data-placement={menuPlacement()?.side ?? "below"}
                style={{
                  left: `${menuPlacement()?.x ?? -9999}px`,
                  top: `${menuPlacement()?.y ?? -9999}px`,
                  visibility: menuPlacement() ? "visible" : "hidden",
                }}
              >
                <For each={PATCH_ACTIONS}>
                  {(action, index) => (
                    <button
                      type="button"
                      role="menuitem"
                      id={`${patchMenuId}-item-${index()}`}
                      data-action={action.id}
                      class="synth-patch-menu-item"
                      classList={{ active: menuActive() === index() }}
                      tabIndex={-1}
                      onMouseEnter={() => setMenuActive(index())}
                      onClick={() => runMenuAction(index())}
                    >
                      {i18n.t(action.labelKey)}
                    </button>
                  )}
                </For>
              </div>
            </Portal>
          </Show>
          <Show when={dialog()}>
            <PatchActionDialog
              title={dialogTitle()}
              initial={dialogInitial()}
              confirmLabel={dialogConfirmLabel()}
              onConfirm={confirmDialog}
              onClose={() => {
                setDialog(null);
                patchMenuTrigger?.focus({ preventScroll: true });
              }}
            />
          </Show>
        </div>
        <Show when={props.libraryWarnings().length > 0}>
          <p class="synth-library-warning selectable" role="status">
            {i18n.t("synth.ui.libraryWarnings", { message: props.libraryWarnings().join(" · ") })}
          </p>
        </Show>
        <Show when={browserOpen()}>
          <FactoryPatchBrowser
            anchor={() => patchField}
            current={props.preset.patch ?? ""}
            chosen={chosenPatch}
            abA={props.preset.name}
            patches={patchRows()}
            auditioning={props.auditioning}
            auditionStatus={props.auditionStatus}
            auditionPaused={props.auditionPaused}
            loading={busy}
            ab={props.abPair}
            abBusy={props.abBusy}
            abError={props.abError}
            onCompare={comparePatch}
            onSwitchAb={() => { void props.switchAb?.(); }}
            onLoad={(name) => { const info = findPatch(name); if (info) void loadPatch(info); }}
            onAudition={auditionPatch}
            onStopAudition={() => void props.stopAudition()}
            onClose={closeBrowser}
          />
        </Show>
        <nav class="synth-view-tabs" aria-label={i18n.t("synth.ui.views")}>
          <For each={["sound", "mod", "fx", "voice"] as View[]}>
            {(tab) => <button classList={{ active: view() === tab }} onClick={() => { releaseAudition(); setView(tab); }}>{i18n.t(`synth.ui.${tab}`).toUpperCase()}</button>}
          </For>
        </nav>
        <button class="synth-expand-button" aria-label={i18n.t(expanded() ? "synth.ui.collapseLabel" : "synth.ui.expandLabel")} onClick={() => { releaseAudition(); setExpanded(!expanded()); }}>
          {i18n.t(expanded() ? "synth.ui.compact" : "synth.ui.expand").toUpperCase()}
        </button>
      </header>

      <div class="synth-performance-toolbar">
        <div class="synth-automation-bar">
          <button
            type="button"
            class="synth-write-arm"
            aria-label={i18n.t("synth.automation.writeArm")}
            aria-pressed={writeArmed()}
            title={i18n.t("automation.write.armHint")}
            onClick={() => toggleAutomationWriteArm()}
          >W</button>
          <span class="synth-automation-mode">
            {writeArmed() ? i18n.t("synth.automation.writeArmed") : ""}
          </span>
          <Show when={automatedTargets().length > 0}>
            <div class="synth-automation-targets" aria-label={i18n.t("synth.automation.lanes")}>
              <For each={automatedTargets()}>
                {(lane) => <span class="synth-automation-target" title={lane.label}>{lane.label}</span>}
              </For>
            </div>
          </Show>
        </div>

        <ControlLinkEditor />

        <Show when={expanded()}>
        <div class="synth-learn-bar" data-learn-ui>
          <button
            type="button"
            class="synth-learn-toggle"
            aria-pressed={learnArmed()}
            onClick={() => {
              setLearnArmed((armed) => !armed);
              setLearnMessage(null);
            }}
            {...useTooltip(() => i18n.t("synth.learn.modeHint"))}
          >{i18n.t("synth.learn.mode")}</button>
          <Dropdown
            class="synth-learn-target"
            label={i18n.t("synth.learn.target")}
            hideLabel
            value={learnMacro()}
            options={Array.from({ length: 8 }, (_, index) => ({
              value: index + 1,
              label: i18n.t("synth.learn.macroOption", { n: index + 1 }),
            }))}
            onSelect={(next) => {
              setLearnMacro(next);
              setLearnMessage(null);
            }}
            triggerProps={useTooltip(() => i18n.t("synth.learn.targetHint"))}
          />
          <span class="synth-learn-status" role="status" aria-live="polite">{learnStatusText()}</span>
        </div>
        </Show>
      </div>

      <Show when={expanded() && hoveredDestination() !== null && !(view() === "mod" && modPage() === "matrix")}>
        <div class="synth-mod-preview" aria-hidden="true" inert>
          <ModRouteGraph
            routes={activeRoutes().filter((route) => route.destination === hoveredDestination())}
            selected={null}
            highlight={hoveredDestination()}
            onSelect={() => {}}
            onAdd={() => {}}
          />
        </div>
      </Show>

      <Show when={patchIssue()}>
        {issue => <p class="synth-patch-error selectable" role="alert">{i18n.t(issue().key, { message: issue().message })}</p>}
      </Show>

      <Show when={expanded()} fallback={
        <div class="synth-compact-strip">
          <SynthWaveDisplay preset={props.preset} drafts={drafts()} oscillator={0}
            waveform={value("osc.a.waveform", 1)} label={i18n.t("synth.ui.oscillator", { osc: "A" })} />
          <div><b>{synthOptionLabel(WAVEFORMS[Math.round(value("osc.a.waveform", 1))] ?? WAVEFORMS[0])}</b><small>{i18n.t("synth.ui.oscillator", { osc: "A" })}</small></div>
          <div><b>{Math.round(value("filter.cutoff", 12000))} Hz</b><small>{i18n.t("synth.ui.filter")}</small></div>
          <div><b>{Math.round(value("unison.detune", 7))} ct</b><small>{i18n.t("synth.ui.detune")}</small></div>
        </div>
      }>
        <Show when={view() === "sound"}>
          <nav class="synth-subtabs" aria-label={i18n.t("synth.ui.soundPages")}>
            <For each={[
              { id: "osc", labelKey: "synth.ui.oscTab" },
              { id: "filter", labelKey: "synth.ui.filter" },
              { id: "amp", labelKey: "synth.ui.ampTab" },
            ] as { id: SoundPage; labelKey: I18nKey }[]}>
              {(page) => (
                <button aria-pressed={soundPage() === page.id} onClick={() => setSoundPage(page.id)}>{i18n.t(page.labelKey).toUpperCase()}</button>
              )}
            </For>
            <button class="synth-subtab-link" aria-label={i18n.t("synth.openModView")} onClick={() => { releaseAudition(); setView("mod"); }} {...useTooltip(() => i18n.t("synth.openModView"))}>{i18n.t("synth.ui.mod").toUpperCase()} ↗</button>
            <span class="synth-subtab-readout">{i18n.t("synth.ui.soundPage", { page: i18n.t(soundPage() === "osc" ? "synth.ui.oscTab" : soundPage() === "amp" ? "synth.ui.ampTab" : "synth.ui.filter") }).toUpperCase()}</span>
          </nav>

          <div class="synth-sound-page" data-page={soundPage()}>
            <Show when={soundPage() === "osc"}>
              <div class="synth-osc-grid">
                <article class="synth-osc-module" data-osc="a">
                  <header><span>{i18n.t("synth.ui.oscillator", { osc: "A" })}</span></header>
                  <div class="synth-osc-top">
                    <SynthWaveDisplay preset={props.preset} drafts={drafts()} oscillator={0}
                      waveform={value("osc.a.waveform", 1)} label={i18n.t("synth.ui.oscillator", { osc: "A" })} />
                    <Select name="osc.a.waveform" label={paramLabel("osc.a.waveform")} options={WAVEFORMS} fallback={1} />
                    <ValueSelect name="unison.voices" label={paramLabel("unison.voices")} options={UNISON_VOICES} fallback={2} />
                  </div>
                  <div class="synth-osc-controls">
                    <div class="synth-param-row">
                      {Range({ name: "osc.a.level", label: paramLabel("osc.a.level") })}
                      {Range({ name: "unison.detune", label: paramLabel("unison.detune") })}
                      {Range({ name: "osc.a.pulse_width", label: paramLabel("osc.a.pulse_width") })}
                      {Range({ name: "osc.a.wavetable_position", label: paramLabel("osc.a.wavetable_position") })}
                      <Select name="osc.a.warp_mode" label={paramLabel("osc.a.warp_mode")} options={WARP_MODES} fallback={0} compact={true} />
                      {Range({ name: "osc.a.warp_amount", label: paramLabel("osc.a.warp_amount") })}
                    </div>
                    <div class="synth-param-row">
                      {CatalogRange("osc.a.warp_var")}
                      {CatalogRange("osc.a.pan")}
                      {CatalogSelect("osc.a.warp2_mode")}
                      {CatalogRange("osc.a.warp2_amount")}
                      {CatalogRange("osc.a.warp2_var")}
                    </div>
                  </div>
                </article>
                <article class="synth-osc-module" data-osc="b">
                  <header><span>{i18n.t("synth.ui.oscillator", { osc: "B" })}</span></header>
                  <div class="synth-osc-top">
                    <SynthWaveDisplay preset={props.preset} drafts={drafts()} oscillator={1}
                      waveform={value("osc.b.waveform", 2)} label={i18n.t("synth.ui.oscillator", { osc: "B" })} />
                    <Select name="osc.b.waveform" label={paramLabel("osc.b.waveform")} options={WAVEFORMS} fallback={2} />
                    <ValueSelect name="osc.b.unison" label={paramLabel("osc.b.unison")} options={UNISON_VOICES} fallback={1} />
                  </div>
                  <div class="synth-osc-controls">
                    <div class="synth-param-row">
                      {Range({ name: "osc.b.level", label: paramLabel("osc.b.level") })}
                      {Range({ name: "osc.b.semitones", label: paramLabel("osc.b.semitones") })}
                      {Range({ name: "osc.b.detune", label: paramLabel("osc.b.detune") })}
                      {Range({ name: "osc.b.width", label: paramLabel("osc.b.width") })}
                      {Range({ name: "osc.b.pan", label: paramLabel("osc.b.pan") })}
                      {Range({ name: "osc.b.pulse_width", label: paramLabel("osc.b.pulse_width") })}
                      {Range({ name: "osc.b.wavetable_position", label: paramLabel("osc.b.wavetable_position") })}
                      <Select name="osc.b.warp_mode" label={paramLabel("osc.b.warp_mode")} options={WARP_MODES} fallback={0} compact={true} />
                      {Range({ name: "osc.b.warp_amount", label: paramLabel("osc.b.warp_amount") })}
                    </div>
                    <div class="synth-param-row">
                      {CatalogRange("osc.b.warp_var")}
                      {CatalogSelect("osc.b.warp2_mode")}
                      {CatalogRange("osc.b.warp2_amount")}
                      {CatalogRange("osc.b.warp2_var")}
                    </div>
                  </div>
                </article>
                <article class="synth-osc-module" data-osc="c">
                  <header><span>{i18n.t("synth.ui.oscillator", { osc: "C" })}</span></header>
                  <div class="synth-osc-top">
                    <SynthWaveDisplay preset={props.preset} drafts={drafts()} oscillator={2}
                      waveform={value("osc.c.waveform", 1)} label={i18n.t("synth.ui.oscillator", { osc: "C" })} />
                    <Select name="osc.c.waveform" label={paramLabel("osc.c.waveform")} options={WAVEFORMS} fallback={1} />
                    <ValueSelect name="osc.c.unison" label={paramLabel("osc.c.unison")} options={UNISON_VOICES} fallback={1} />
                  </div>
                  <div class="synth-osc-controls">
                    <div class="synth-param-row">
                      {Range({ name: "osc.c.level", label: paramLabel("osc.c.level") })}
                      {Range({ name: "osc.c.semitones", label: paramLabel("osc.c.semitones") })}
                      {Range({ name: "osc.c.detune", label: paramLabel("osc.c.detune") })}
                      {Range({ name: "osc.c.width", label: paramLabel("osc.c.width") })}
                      {Range({ name: "osc.c.pan", label: paramLabel("osc.c.pan") })}
                      {Range({ name: "osc.c.pulse_width", label: paramLabel("osc.c.pulse_width") })}
                      {Range({ name: "osc.c.wavetable_position", label: paramLabel("osc.c.wavetable_position") })}
                      <Select name="osc.c.warp_mode" label={paramLabel("osc.c.warp_mode")} options={WARP_MODES} fallback={0} compact={true} />
                      {Range({ name: "osc.c.warp_amount", label: paramLabel("osc.c.warp_amount") })}
                    </div>
                    <div class="synth-param-row">
                      {CatalogRange("osc.c.warp_var")}
                      {CatalogSelect("osc.c.warp2_mode")}
                      {CatalogRange("osc.c.warp2_amount")}
                      {CatalogRange("osc.c.warp2_var")}
                    </div>
                  </div>
                </article>
              </div>
              <div class="synth-sample-strip">
                <div class="synth-file-control">
                  <button onClick={() => void browseWavetable()}>{i18n.t("synth.ui.loadWavetable").toUpperCase()}</button>
                  <span {...useTooltip(() => storedString(props.preset, "wavetable.file") ?? i18n.t("synth.ui.internalWavetable"))}>
                    {storedString(props.preset, "wavetable.file")?.split(/[\\/]/).pop() ?? i18n.t("synth.ui.internalBank")}
                  </span>
                  <Show when={storedString(props.preset, "wavetable.file")}>
                    <button aria-label={i18n.t("synth.ui.clearWavetable")} onClick={() => void setBatch([["wavetable.file", null]])}>×</button>
                  </Show>
                </div>
                <div class="synth-file-control">
                  <button
                    aria-label={i18n.t("synth.loadExternal")}
                    onClick={() => void importPreset()}
                    {...useTooltip(() => i18n.t("synth.loadExternal"))}
                  >{i18n.t("synth.ui.importPreset").toUpperCase()}</button>
                  <span {...useTooltip(() => storedString(props.preset, "serum_file") ?? i18n.t("synth.ui.importedPreset"))}>
                    {storedString(props.preset, "serum_file")?.split(/[\\/]/).pop() ?? i18n.t("synth.ui.noImportedPreset")}
                  </span>
                  <Show when={storedString(props.preset, "serum_file")}>
                    <button aria-label={i18n.t("synth.ui.clearImported")} onClick={() => void setBatch([["serum_file", null]])}>×</button>
                  </Show>
                </div>
              </div>
            </Show>

            <Show when={soundPage() === "filter"}>
              <div class="synth-filter-grid">
                <article class="synth-filter-module">
                  <header><span>{i18n.t("synth.ui.multimodeFilter").toUpperCase()}</span></header>
                  <FilterDisplay cutoff={value("filter.cutoff", 12000)} resonance={value("filter.resonance", 0.15)} mode={Math.round(value("filter.mode", 0))} model={Math.round(value("filter.model", 0))} />
                  <div class="synth-compact-selects">
                    <Select name="filter.model" label={paramLabel("filter.model")} options={FILTER_MODELS} fallback={0} />
                    <Select name="filter.mode" label={paramLabel("filter.mode")} options={FILTER_MODES} fallback={0} />
                  </div>
                  <div class="synth-param-row">
                    {Range({ name: "filter.cutoff", label: paramLabel("filter.cutoff") })}
                    {Range({ name: "filter.resonance", label: paramLabel("filter.resonance") })}
                    {Range({ name: "filter.drive", label: paramLabel("filter.drive") })}
                    {Range({ name: "filter.keytrack", label: paramLabel("filter.keytrack") })}
                    {CatalogRange("filter.wet")}
                    {Range({ name: "filter.env.amount", label: paramLabel("filter.env.amount") })}
                  </div>
                </article>
                <article class="synth-filter-module" data-enabled={value("filter2.enabled", 0) >= 0.5 ? "on" : "off"}>
                  <header><span>{i18n.t("synth.ui.secondFilter").toUpperCase()}</span><i>{i18n.t(value("filter.routing", 0) >= 0.5 ? "synth.ui.parallel" : "synth.ui.serial")}</i></header>
                  <FilterDisplay cutoff={value("filter2.cutoff", 12000)} resonance={value("filter2.resonance", 0)} mode={Math.round(value("filter2.mode", 0))} model={Math.round(value("filter2.model", 0))} label={i18n.t("synth.ui.secondFilter")} />
                  <div class="synth-compact-selects">
                    <Select name="filter2.model" label={paramLabel("filter2.model")} options={FILTER_MODELS} fallback={0} />
                    <Select name="filter2.mode" label={paramLabel("filter2.mode")} options={FILTER_MODES} fallback={0} />
                  </div>
                  <div class="synth-param-row">
                    {Range({ name: "filter2.cutoff", label: paramLabel("filter2.cutoff") })}
                    {Range({ name: "filter2.resonance", label: paramLabel("filter2.resonance") })}
                    {Range({ name: "filter2.drive", label: paramLabel("filter2.drive") })}
                    {CatalogRange("filter2.wet")}
                    {Toggle({ name: "filter2.enabled", label: paramLabel("filter2.enabled"), fallback: 0 })}
                    <ValueSelect name="filter.routing" label={paramLabel("filter.routing")} options={[[0, "Serial"], [1, "Parallel"]]} fallback={0} compact={true} />
                  </div>
                </article>
              </div>
            </Show>

            <Show when={soundPage() === "amp"}>
              <div class="synth-amp-grid">
                <article class="synth-amp-module">
                  <header><span>{i18n.t("synth.ui.sourceMix")}</span></header>
                  <div class="synth-source-meter"><span style={{ width: `${value("osc.a.level", 0.78) * 100}%` }} /></div>
                  <div class="synth-param-row">
                    {Range({ name: "sub.level", label: paramLabel("sub.level") })}
                    {Range({ name: "noise.level", label: paramLabel("noise.level") })}
                    {Range({ name: "ring.level", label: paramLabel("ring.level") })}
                    {Range({ name: "output.gain", label: paramLabel("output.gain") })}
                  </div>
                </article>
                <article class="synth-amp-module">
                  <header><span>{i18n.t("synth.ui.ampEnv").toUpperCase()}</span></header>
                  <EnvelopeDisplay {...amp()} label={i18n.t("synth.ui.amplitude")} />
                  <div class="synth-param-row">
                    {Range({ name: "amp.attack", label: paramLabel("amp.attack") })}
                    {Range({ name: "amp.decay", label: paramLabel("amp.decay") })}
                    {Range({ name: "amp.sustain", label: paramLabel("amp.sustain") })}
                    {Range({ name: "amp.release", label: paramLabel("amp.release") })}
                    {Range({ name: "amp.curve", label: paramLabel("amp.curve") })}
                  </div>
                </article>
                <article class="synth-amp-module">
                  <header><span>{i18n.t("synth.ui.filterEnv").toUpperCase()}</span></header>
                  <EnvelopeDisplay {...filterEnv()} label={i18n.t("synth.ui.filter")} />
                  <div class="synth-param-row">
                    {Range({ name: "filter_env.attack", label: paramLabel("filter_env.attack") })}
                    {Range({ name: "filter_env.decay", label: paramLabel("filter_env.decay") })}
                    {Range({ name: "filter_env.sustain", label: paramLabel("filter_env.sustain") })}
                    {Range({ name: "filter_env.release", label: paramLabel("filter_env.release") })}
                    {Range({ name: "filter_env.curve", label: paramLabel("filter_env.curve") })}
                  </div>
                </article>
                <article class="synth-amp-module">
                  <header><span>LFO 1</span></header>
                  <LfoCurveDisplay
                    shapeType={Math.round(value("lfo.shape", 0))}
                    xVals={storedArray(props.preset, "lfo.shape.x")}
                    yVals={storedArray(props.preset, "lfo.shape.y")}
                    curveVals={storedArray(props.preset, "lfo.shape.curve")}
                    points={Math.round(value("lfo.shape.points", 3))}
                    label="LFO 1"
                  />
                  <div class="synth-param-row">
                    {Range({ name: "lfo.rate", label: paramLabel("lfo.rate") })}
                    {Range({ name: "lfo.phase", label: paramLabel("lfo.phase") })}
                    <Select name="lfo.shape" label={paramLabel("lfo.shape")} options={LFO_SHAPES} fallback={0} compact={true} />
                    <Select name="lfo.sync_div" label={paramLabel("lfo.sync_div")} options={LFO_DIVISIONS} fallback={22} compact={true} />
                  </div>
                  <div class="synth-param-row">
                    {Range({ name: "lfo.smooth", label: paramLabel("lfo.smooth") })}
                    {Range({ name: "lfo.rise", label: paramLabel("lfo.rise") })}
                    {Range({ name: "lfo.delay", label: paramLabel("lfo.delay") })}
                    {Range({ name: "lfo.pitch", label: paramLabel("lfo.pitch") })}
                  </div>
                  <div class="synth-toggle-row">
                    {Toggle({ name: "lfo.sync", label: paramLabel("lfo.sync"), fallback: 0 })}
                    {Toggle({ name: "lfo.retrigger", label: paramLabel("lfo.retrigger"), fallback: 0 })}
                    {Toggle({ name: "lfo.rate_x10", label: paramLabel("lfo.rate_x10"), fallback: 0 })}
                  </div>
                </article>
              </div>
            </Show>
          </div>

          <section class="synth-audition synth-audition-footer" aria-label={i18n.t("synth.ui.previewKeyboard")}>
            <header><span>{i18n.t("synth.ui.play").toUpperCase()}</span><small {...useTooltip(() => i18n.t("synth.dryAudition"))}>{i18n.t("synth.dryAudition")}</small></header>
            <div class="synth-keyboard">
              <For each={AUDITION_KEYS}>{([note, label]) => (
                <button
                  classList={{ black: label.includes("#"), active: activeNotes().has(note) }}
                  aria-label={i18n.t("synth.ui.previewNote", { note: label })}
                  aria-pressed={activeNotes().has(note)}
                  {...useTooltip(() => i18n.t("synth.ui.previewNote", { note: label }))}
                  onPointerDown={(event) => {
                    event.preventDefault();
                    event.currentTarget.setPointerCapture?.(event.pointerId);
                    startNote(note);
                  }}
                  onPointerUp={(event) => {
                    stopNote(note);
                    event.currentTarget.releasePointerCapture?.(event.pointerId);
                  }}
                  onPointerCancel={() => stopNote(note)}
                  onLostPointerCapture={() => stopNote(note)}
                  onBlur={() => stopNote(note)}
                  onKeyDown={(event) => {
                    if ((event.key === " " || event.key === "Enter") && !event.repeat) {
                      event.preventDefault();
                      startNote(note);
                    }
                  }}
                  onKeyUp={(event) => {
                    if (event.key === " " || event.key === "Enter") {
                      event.preventDefault();
                      stopNote(note);
                    }
                  }}
                ><span>{label}</span></button>
              )}</For>
            </div>
          </section>
        </Show>

        <Show when={view() === "mod"}>
          <nav class="synth-module-tabs" aria-label={i18n.t("synth.ui.modPages")}>
            <For each={["matrix", "routing", "macros", "steps", "envelopes", "lfos"] as const}>
              {(id) => <button aria-pressed={modPage() === id} onClick={() => setModPage(id)}>{i18n.t(`synth.ui.${id}`).toUpperCase()}</button>}
            </For>
          </nav>
          <div class="synth-mod-view" data-page={modPage()}>
            <div class="synth-direct-routing">
              <section>
                <header>{i18n.t("synth.ui.sources").toUpperCase()}</header>
                <div class="synth-routing-options">
                  <For each={MOD_SOURCES}>{(source, index) => (
                    <button
                      draggable
                      classList={{ active: armedSource() === index() }}
                      aria-label={i18n.t("synth.ui.armSource", { source: synthOptionLabel(source) })}
                      onClick={() => setArmedSource(armedSource() === index() ? null : index())}
                      onDragStart={(event) => {
                        event.dataTransfer?.setData("text/opencompose-mod-source", String(index()));
                        setArmedSource(index());
                      }}
                    >{synthOptionLabel(source)}</button>
                  )}</For>
                </div>
              </section>
              <span class="synth-route-arrow" aria-hidden="true">→</span>
              <section>
                <header>{i18n.t("synth.ui.targets").toUpperCase()}</header>
                <div class="synth-routing-options targets">
                  <For each={MOD_DESTINATIONS}>{(destination, index) => (
                    <button
                      aria-label={i18n.t("synth.ui.routeTo", { destination: synthOptionLabel(destination) })}
                      disabled={armedSource() === null}
                      onClick={() => armedSource() !== null && void assignRoute(armedSource()!, index())}
                      onDragOver={(event) => event.preventDefault()}
                      onDrop={(event) => {
                        event.preventDefault();
                        const raw = event.dataTransfer?.getData("text/opencompose-mod-source");
                        if (raw?.trim()) void assignRoute(Number(raw), index());
                      }}
                    >{synthOptionLabel(destination)}</button>
                  )}</For>
                </div>
              </section>
            </div>
            <Show when={routeError()}><p class="synth-route-error selectable" role="alert">{i18n.t("synth.matrixFull")}</p></Show>
            <div class="synth-macro-bank">
              <For each={[1, 2, 3, 4, 5, 6, 7, 8]}>{(macro) => (
                <div class="synth-macro">
                  {Range({ name: `macro.${macro}`, label: i18n.t("synth.learn.macroOption", { n: macro }) })}
                </div>
              )}</For>
            </div>
            <section class="synth-step-sequencer" aria-label={i18n.t("synth.ui.stepMod")}>
              <header>
                <div><b>{i18n.t("synth.ui.stepMod").toUpperCase()}</b></div>
                <div class="synth-step-options">
                  {Range({ name: "step.rate", label: paramLabel("step.rate") })}
                  {Toggle({ name: "step.sync", label: paramLabel("step.sync"), fallback: 1 })}
                  <Select name="step.division" label={paramLabel("step.division")} options={["1/16", "1/8", "1/4", "1/2", "1 bar", "2 bars"]} fallback={0} />
                  {Range({ name: "step.smoothing", label: paramLabel("step.smoothing") })}
                </div>
              </header>
              <div class="synth-step-grid">
                <For each={Array.from({ length: 16 }, (_, index) => index + 1)}>{(step) => {
                  const key = `step.${step}`;
                  const spec = specForConcreteKey(key)!;
                  const gesture = rangeGesture(key, () => value(key, spec.default));
                  return (<label
                    class="synth-step"
                    data-param={key}
                    data-automation-value={String(value(key, spec.default))}
                  >
                    <output>{formatParameterValue(value(key, spec.default), spec.step, spec.unit)}</output>
                    <input
                      aria-label={i18n.t("synth.ui.stepNumber", { n: step })}
                      aria-valuetext={formatParameterValue(value(key, spec.default), spec.step, spec.unit)}
                      type="range"
                      min={spec.min}
                      max={spec.max}
                      step={spec.step}
                      value={value(key, spec.default)}
                      disabled={busy()}
                      onPointerUp={focusRange}
                      onClick={focusRange}
                      onBlur={() => gesture.interrupt()}
                      onPointerDown={(event) => { if (!busy() && !learnArmed()) gesture.pointerDown(event); }}
                      onKeyDown={(event) => {
                        if (busy() || event.metaKey || event.ctrlKey || event.altKey) return;
                        const next = parameterKeyValue(spec, value(key, spec.default), event.key);
                        if (next === null) return;
                        event.preventDefault();
                        gesture.discrete(next);
                      }}
                      onInput={(event) => {
                        if (busy() || learnArmed()) return;
                        gesture.input(parameterValue(spec, Number(event.currentTarget.value)));
                      }}
                      onChange={(event) => {
                        if (busy() || learnArmed()) return;
                        gesture.change(parameterValue(spec, Number(event.currentTarget.value)));
                      }}
                      onPointerCancel={(event) => gesture.pointerCancel(event)}
                      onDblClick={(event) => resetRange(event, gesture, spec.default)}
                    />
                    <span>{String(step).padStart(2, "0")}</span>
                  </label>
                ); }}</For>
              </div>
            </section>
            <div class="synth-env-bank" aria-label={i18n.t("synth.ui.envEditors")}>
              <For each={[3, 4]}>{(env) => (
                <article class="synth-module-card synth-env-card" data-env={env} aria-label={i18n.t("synth.ui.env", { n: env })}>
                  <header><span>{i18n.t("synth.ui.modEnvelope").toUpperCase()}</span><i>{i18n.t("synth.ui.env", { n: env })}</i></header>
                  <EnvelopeDisplay
                    attack={value(`env${env}.attack`, 0.01)}
                    decay={value(`env${env}.decay`, 0.4)}
                    sustain={value(`env${env}.sustain`, 0.5)}
                    release={value(`env${env}.release`, 0.5)}
                    curve={value(`env${env}.curve`, 0)}
                    label={`Env ${env}`}
                  />
                  <div class="synth-mini-controls">
                    {Range({ name: `env${env}.attack`, label: paramLabel(`env${env}.attack`) })}
                    {Range({ name: `env${env}.decay`, label: paramLabel(`env${env}.decay`) })}
                    {Range({ name: `env${env}.sustain`, label: paramLabel(`env${env}.sustain`) })}
                    {Range({ name: `env${env}.release`, label: paramLabel(`env${env}.release`) })}
                    {Range({ name: `env${env}.curve`, label: paramLabel(`env${env}.curve`) })}
                  </div>
                </article>
              )}</For>
            </div>
            <div class="synth-lfo-bank" aria-label={i18n.t("synth.ui.lfoEditors")}>
              <section class="synth-lfo-picker">
                <header>{i18n.t("synth.ui.lfoBank").toUpperCase()}</header>
                <Dropdown
                  label={i18n.t("synth.ui.lfo")}
                  class="synth-select-control"
                  value={lfoIndex()}
                  options={Array.from({ length: LFO_COUNT }, (_, index) => ({ value: index + 1, label: `LFO ${index + 1}` }))}
                  onSelect={(next) => setLfoIndex(next)}
                />

              </section>
              <Show when={lfoIndex()} keyed>
                {(selected) => (
                  <article class="synth-module-card synth-lfo-card" aria-label={i18n.t("synth.ui.lfoEditor", { n: selected })}>
                    <header><i>{`LFO ${selected}`}</i></header>
                    <LfoCurveDisplay
                      shapeType={Math.round(value(lfoParamName(selected, "shape"), 0))}
                      xVals={storedArray(props.preset, lfoParamName(selected, "shape.x"))}
                      yVals={storedArray(props.preset, lfoParamName(selected, "shape.y"))}
                      curveVals={storedArray(props.preset, lfoParamName(selected, "shape.curve"))}
                      points={Math.round(value(lfoParamName(selected, "shape.points"), 3))}
                      label={`LFO ${selected}`}
                    />
                    {Range({ name: lfoParamName(selected, "rate"), label: paramLabel(lfoParamName(selected, "rate")) })}
                    <Show when={selected === 1}>
                      {Range({ name: "lfo.pitch", label: paramLabel("lfo.pitch") })}
                    </Show>
                    <Select name={lfoParamName(selected, "shape")} label={paramLabel(lfoParamName(selected, "shape"))} options={LFO_SHAPES} fallback={0} />
                    {Range({ name: lfoParamName(selected, "phase"), label: paramLabel(lfoParamName(selected, "phase")) })}
                    {Range({ name: lfoParamName(selected, "smooth"), label: paramLabel(lfoParamName(selected, "smooth")) })}
                    {Range({ name: lfoParamName(selected, "rise"), label: paramLabel(lfoParamName(selected, "rise")) })}
                    {Range({ name: lfoParamName(selected, "delay"), label: paramLabel(lfoParamName(selected, "delay")) })}
                    <div class="synth-inline-options">
                      {Toggle({ name: lfoParamName(selected, "sync"), label: paramLabel(lfoParamName(selected, "sync")), fallback: 0 })}
                      {Toggle({ name: lfoParamName(selected, "retrigger"), label: paramLabel(lfoParamName(selected, "retrigger")), fallback: 0 })}
                      {Toggle({ name: lfoParamName(selected, "rate_x10"), label: "Rate ×10", fallback: 0 })}
                      <Select name={lfoParamName(selected, "sync_div")} label={paramLabel(lfoParamName(selected, "sync_div"))} options={LFO_DIVISIONS} fallback={22} />
                    </div>
                  </article>
                )}
              </Show>
            </div>
            <ModRouteGraph
              routes={activeRoutes()}
              selected={selectedRoute()}
              highlight={hoveredDestination()}
              onSelect={setSelectedRoute}
              onAdd={() => setModPage("routing")}
            />
            <Show when={selectedRoute()}>
              {(slot) => (
                <div class="synth-route-inspector" data-slot={slot()} aria-label={i18n.t("synth.routeInspector", { slot: slot() })} role="group">
                  <b>{String(slot()).padStart(2, "0")}</b>
                  <Select name={`mod.${slot()}.source`} label={i18n.t("synth.ui.routeSource", { slot: slot() })} options={MOD_SOURCES} fallback={0} />
                  <span class="synth-route-arrow" aria-hidden="true">→</span>
                  <Select name={`mod.${slot()}.destination`} label={i18n.t("synth.ui.routeDestination", { slot: slot() })} options={MOD_DESTINATIONS} fallback={0} />
                  {Range({ name: `mod.${slot()}.amount`, label: i18n.t("synth.ui.routeAmount", { slot: slot() }) })}
                  <button class="synth-route-remove" aria-label={i18n.t("synth.removeRoute", { slot: slot() })} onClick={() => void removeRoute(slot())}>×</button>
                  <div class="synth-route-attrs" role="group" aria-label={i18n.t("synth.attr.group", { slot: slot() })}>
                    {Toggle({
                      name: `mod.${slot()}.bipolar`,
                      label: i18n.t("synth.attr.bipolar"),
                      fallback: 0,
                      hint: () => i18n.t("synth.attr.bipolarHint"),
                    })}
                    {Range({ name: `mod.${slot()}.curve`, label: i18n.t("synth.attr.curve"), hint: () => i18n.t("synth.attr.curveHint") })}
                    {Range({ name: `mod.${slot()}.slew_up`, label: i18n.t("synth.attr.slewUp"), hint: () => i18n.t("synth.attr.slewHint") })}
                    {Range({ name: `mod.${slot()}.slew_down`, label: i18n.t("synth.attr.slewDown"), hint: () => i18n.t("synth.attr.slewHint") })}
                    {Select({
                      name: `mod.${slot()}.aux`,
                      label: i18n.t("synth.attr.aux"),
                      options: MOD_AUX,
                      fallback: 0,
                      hint: () => i18n.t("synth.attr.auxHint"),
                    })}
                    {Toggle({
                      name: `mod.${slot()}.aux_invert`,
                      label: i18n.t("synth.attr.auxInvert"),
                      fallback: 0,
                      disabled: () => Math.round(value(`mod.${slot()}.aux`, 0)) <= 0,
                      hint: () =>
                        Math.round(value(`mod.${slot()}.aux`, 0)) <= 0
                          ? i18n.t("synth.attr.auxInvertDisabled")
                          : i18n.t("synth.attr.auxInvertHint"),
                    })}
                  </div>
                </div>
              )}
            </Show>
            <div class="synth-slot-row">
            <details class="synth-slot-list">
              <summary>{i18n.t("synth.slotList")} <span class="synth-matrix-count">{`${usedRoutes()} / ${MATRIX_SLOTS}`}</span></summary>
            <div class="synth-matrix-toolbar">
              <div class="synth-matrix-pages">
                <button
                  aria-label={i18n.t("synth.ui.prevSlots")}
                  disabled={matrixPage() === 0}
                  onClick={() => setMatrixPage((page) => Math.max(0, page - 1))}
                >{i18n.t("synth.ui.prev").toUpperCase()}</button>
                <For each={Array.from({ length: MATRIX_PAGES }, (_, page) => page)}>{(page) => (
                  <button
                    aria-label={i18n.t("synth.ui.slotPage", { first: page * MATRIX_PAGE_SIZE + 1, last: (page + 1) * MATRIX_PAGE_SIZE })}
                    aria-pressed={matrixPage() === page}
                    onClick={() => setMatrixPage(page)}
                  >{`${String(page * MATRIX_PAGE_SIZE + 1).padStart(2, "0")}–${(page + 1) * MATRIX_PAGE_SIZE}`}</button>
                )}</For>
                <button
                  aria-label={i18n.t("synth.ui.nextSlots")}
                  disabled={matrixPage() === MATRIX_PAGES - 1}
                  onClick={() => setMatrixPage((page) => Math.min(MATRIX_PAGES - 1, page + 1))}
                >{i18n.t("synth.ui.next").toUpperCase()}</button>
              </div>
              <span class="synth-matrix-count">{i18n.t("synth.ui.slots", { first: matrixPage() * MATRIX_PAGE_SIZE + 1, last: (matrixPage() + 1) * MATRIX_PAGE_SIZE, total: MATRIX_SLOTS })}</span>
              <span class="synth-matrix-count">{i18n.t("synth.ui.routes", { used: usedRoutes(), total: MATRIX_SLOTS })}</span>
            </div>
            <div class="synth-matrix" role="table" aria-label={i18n.t("synth.ui.modMatrix")}>
              <div class="synth-matrix-header" role="row"><span>{i18n.t("synth.ui.slot").toUpperCase()}</span><span>{i18n.t("synth.ui.source").toUpperCase()}</span><span>{i18n.t("synth.ui.destination").toUpperCase()}</span><span>{i18n.t("synth.ui.amount").toUpperCase()}</span></div>
              <For each={matrixSlots()}>{(slot) => (
                <div class="synth-matrix-row" role="row" data-slot={slot} onClick={() => setSelectedRoute(slot)}>
                  <b>{String(slot).padStart(2, "0")}</b>
                  <Select name={`mod.${slot}.source`} label={i18n.t("synth.ui.routeSource", { slot })} options={MOD_SOURCES} fallback={0} />
                  <Select name={`mod.${slot}.destination`} label={i18n.t("synth.ui.routeDestination", { slot })} options={MOD_DESTINATIONS} fallback={0} />
                  {Range({ name: `mod.${slot}.amount`, label: i18n.t("synth.ui.routeAmount", { slot }) })}
                </div>
              )}</For>
            </div>
            </details>
            <button class="synth-route-add" onClick={() => setModPage("routing")} aria-label={i18n.t("synth.addRoute")}>+ {i18n.t("synth.addRoute")}</button>
            </div>
          </div>
        </Show>

        <Show when={view() === "fx"}>
          <div class="synth-fx-view">

            <SynthFxEditor preset={props.preset} setParams={async (params, replaceSynth) => {
              const error = await setBatch(params, replaceSynth);
              setPatchError(error ?? "");
              return error;
            }} />
            <div class="synth-view-intro"><b>{i18n.t("synth.ui.presetInserts")}</b></div>
            <EffectsRack
              target={`preset:${props.preset.name}`}
              effects={props.authoredEffects ?? props.preset.effects}
              inheritedEffects={props.authoredEffects
                ? props.preset.effects.slice(0, props.preset.effects.length - props.authoredEffects.length)
                : undefined}
              label={i18n.t("synth.ui.effectChain")}
            />
          </div>
        </Show>

        <Show when={view() === "voice"}>
          <div class="synth-voice-view">
            <div class="synth-voice-readout"><strong>{value("voice.polyphony", 16)}</strong><span>{i18n.t("synth.ui.voicePolyphony").toUpperCase()}</span></div>
            <div class="synth-voice-controls">
              {Toggle({ name: "voice.mono", label: paramLabel("voice.mono"), fallback: 0 })}
              <ValueSelect name="voice.polyphony" label={paramLabel("voice.polyphony")} options={[1, 2, 4, 8, 16, 24, 32].map(n => [n, i18n.t(n === 1 ? "synth.ui.voiceOne" : "synth.ui.voiceMany", { n })])} fallback={16} />
              {Toggle({ name: "voice.legato", label: paramLabel("voice.legato"), fallback: 1 })}
              {Range({ name: "voice.glide", label: paramLabel("voice.glide") })}
              <Select name="quality.oversampling" label={paramLabel("quality.oversampling")} options={["Eco · 1×", "Studio · 2×", "Ultra · 4×"]} fallback={1} />
              <ValueSelect name="unison.voices" label={paramLabel("unison.voices")} options={Array.from({ length: 16 }, (_, index) => [index + 1, i18n.t(index === 0 ? "synth.ui.voiceOne" : "synth.ui.voiceMany", { n: index + 1 })] as [number, string])} fallback={2} />
              {Range({ name: "unison.detune", label: paramLabel("unison.detune") })}
              {Range({ name: "unison.width", label: paramLabel("unison.width") })}
              {Range({ name: "osc.start_phase", label: paramLabel("osc.start_phase") })}
              {Range({ name: "osc.random_phase", label: paramLabel("osc.random_phase") })}
              {Range({ name: "tuning.reference_hz", label: paramLabel("tuning.reference_hz") })}
              {Range({ name: "tuning.transpose", label: paramLabel("tuning.transpose") })}
              {Range({ name: "tuning.fine", label: paramLabel("tuning.fine") })}
              {Range({ name: "filter.keytrack", label: paramLabel("filter.keytrack") })}
              {Range({ name: "output.gain", label: paramLabel("output.gain") })}
            </div>

          </div>
        </Show>
      </Show>

      <Show when={affordance()}>
        {(spot) => (
          <button
            type="button"
            class="synth-automate-affordance"
            aria-label={i18n.t("synth.automation.badge", { param: automationLabel(spot().key) })}
            style={{ left: `${spot().x}px`, top: `${spot().y}px` }}
            onPointerDown={(event) => event.stopPropagation()}
            onClick={(event) => {
              event.preventDefault();
              event.stopPropagation();
              const key = spot().key;
              const physical = automationValue(key);
              setAffordance(null);
              void automatePoint(key, physical);
            }}
          >A</button>
        )}
      </Show>
    </section>
  );
}
