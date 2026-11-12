/**
 * Drum-kit document helpers shared by the pad panel and its tests.
 *
 * Kits are stored as `builtin.drumkit` presets whose params use the same
 * `pad.<name>.*` / `layer.<name>.<index>.*` keys that `@kit` parses into.
 */

import type { HitEvent, ParamValue, PresetDef } from "../types/project";

export const DRUMKIT_PLUGIN = "builtin.drumkit";

/** Grid defaults: a General MIDI style 4x4 kit. */
export const DEFAULT_PAD_NOTES = [
  36, 38, 39, 37, 42, 46, 41, 47, 50, 49, 51, 55, 52, 56, 70, 54,
];

export interface KitLayer {
  low: number;
  high: number;
  sample: string;
  gainDb: number;
}

export interface KitPad {
  name: string;
  note: number;
  gainDb: number;
  pan: number;
  tune: number;
  cents: number;
  decay: number;
  release: number;
  reverse: boolean;
  oneShot: boolean;
  choke: number;
  layers: KitLayer[];
}

export interface Kit {
  name: string;
  maxVoices: number;
  pads: KitPad[];
}

/**
 * One row of the piano roll's drum lane: a kit pad in the kit's own order, or
 * an `Other <note>` row for a note the kit does not declare.
 */
export interface DrumLaneRow {
  /** MIDI note the row plays. */
  note: number;
  /** Pad name, or `Other <note>`. */
  label: string;
  /** Pad the row belongs to, or null on an `Other` row. */
  pad: string | null;
}

/** Quarter notes a hit's written `beat:subdivision` position covers. */
export const HIT_STEP_QUARTERS = 0.25;

/** General MIDI percussion notes, the fallback a name with no kit pad uses. */
const GM_DRUM_NOTES: Record<string, number> = {
  kick: 36, bassdrum: 36,
  rim: 37, rimshot: 37, crossstick: 37,
  snare: 38, snaredrum: 38,
  clap: 39, handclap: 39,
  tomlow: 41, lowtom: 41,
  hihat: 42, closedhihat: 42, hihatclosed: 42,
  tommid: 45, midtom: 45,
  hihatopen: 46, openhihat: 46, openhat: 46,
  tomhigh: 48, hightom: 48,
  crash: 49, crashcymbal: 49,
  ride: 51, ridecymbal: 51,
  china: 52, chinacymbal: 52,
  tambourine: 54,
  splash: 55, splashcymbal: 55,
  cowbell: 56,
  cabasa: 69,
  shaker: 70,
  guiro: 73,
  woodblock: 75,
};

/** The engine drops separators and case before matching a drum name. */
export function normalizeDrumName(name: string): string {
  return name.replace(/[^A-Za-z0-9]/g, "").toLowerCase();
}

/** Authored onset of a hit, before expressive microtiming and swing. */
export function hitWrittenQuarters(hit: HitEvent): number {
  const anchor = Math.max(0, hit.position.beat - 1) + hit.position.subdivision * HIT_STEP_QUARTERS;
  return anchor + (hitOffset(hit, "grid_offset_beats") ?? 0);
}

/** Authored editor position, including the signed microtiming offset. */
export function hitStartQuarters(hit: HitEvent): number {
  return hitWrittenQuarters(hit) + (hitTimingOffset(hit) ?? 0);
}

/** Swing shifts only an exact off-beat eighth, using the project beat grid. */
export function swingOffsetQuarters(beat: number, swing?: number | null): number {
  if (swing == null) return 0;
  const eighth = Math.round(beat * 2);
  if (Math.abs(beat * 2 - eighth) > 1e-9 || eighth % 2 === 0) return 0;
  return swing / 100 - 0.5;
}

export type PatternHitWindow = {
  /** Placement boundaries in absolute project quarters. */
  start: number;
  end: number;
  /** Written source offset and repeat period, also in quarters. */
  offset: number;
  loop: number;
  /** Effective pattern or project swing, in percent. */
  swing?: number | null;
};

/** Actual placement onsets. Written eligibility precedes swing and microtiming. */
export function* patternHitOnsets(hit: HitEvent, window: PatternHitWindow): Generator<number> {
  const epsilon = 1e-9;
  const local = hitWrittenQuarters(hit) - window.offset;
  const looping = window.loop > 0;
  if (local < -epsilon || (looping && local >= window.loop - epsilon)) return;
  const microtiming = hitTimingOffset(hit) ?? 0;
  const limit = looping ? 1024 : 1;
  for (let repeat = 0; repeat < limit; repeat++) {
    const written = window.start + local + repeat * window.loop;
    if (written >= window.end - epsilon) break;
    const shifted = Math.max(window.start, written + swingOffsetQuarters(written, window.swing) + microtiming);
    if (shifted < window.end - epsilon) yield shifted;
  }
}

/** A hit's `timing_offset_beats`, or null when it writes none. */
export function hitTimingOffset(hit: HitEvent): number | null {
  return hitOffset(hit, "timing_offset_beats");
}

function hitOffset(hit: HitEvent, name: string): number | null {
  const found = hit.modifiers?.find(([key]) => key === name);
  if (!found || !("Number" in found[1])) return null;
  const value = found[1].Number;
  return Number.isFinite(value) ? value : null;
}

/**
 * The note a hit plays: an explicit `midi_note`, else the kit pad whose name
 * it writes, else the General MIDI note for that name.
 */
export function hitNote(hit: HitEvent, kit: Kit | null): number {
  const written = hit.modifiers?.find(([key]) => key === "midi_note");
  if (written && "Number" in written[1] && Number.isFinite(written[1].Number)) {
    return Math.round(written[1].Number);
  }
  const key = normalizeDrumName(hit.drum);
  const pad = kit?.pads.find((candidate) => normalizeDrumName(candidate.name) === key);
  if (pad) return pad.note;
  return GM_DRUM_NOTES[key] ?? 38;
}

/**
 * The lane's rows: every kit pad in the kit's order, then an `Other <note>` row
 * for each used note the kit does not declare. A semitone the kit does not name
 * and no content uses is not a row, so the lane holds the kit's pads instead of
 * the whole pitch range.
 */
export function kitLaneRows(kit: Kit, usedNotes: Iterable<number>): DrumLaneRow[] {
  const rows: DrumLaneRow[] = [];
  const claimed = new Set<number>();
  for (const pad of kit.pads) {
    if (claimed.has(pad.note)) continue;
    claimed.add(pad.note);
    rows.push({ note: pad.note, label: pad.name, pad: pad.name });
  }
  const others = new Set<number>();
  for (const note of usedNotes) {
    if (!claimed.has(note)) others.add(note);
  }
  for (const note of [...others].sort((left, right) => left - right)) {
    rows.push({ note, label: `Other ${note}`, pad: null });
  }
  return rows;
}

/**
 * The drum name and optional `midi_note` a hit placed on `note` writes: the
 * kit pad that holds the note, else `pad_<note>` carrying the note itself, so
 * the hit sounds whatever the kit maps the note to instead of a fixed GM note.
 */
export function drumForNote(
  note: number,
  kit: Kit | null,
): { drum: string; midiNote: number | null } {
  const pad = kit?.pads.find((candidate) => candidate.note === note);
  return pad ? { drum: pad.name, midiNote: null } : { drum: `pad_${note}`, midiNote: note };
}

export function isDrumKitPreset(preset: Pick<PresetDef, "plugin">): boolean {
  return preset.plugin === DRUMKIT_PLUGIN;
}

export function kitFromPreset(preset: PresetDef): Kit {
  const pads = new Map<string, KitPad>();
  const order: string[] = [];
  const layers = new Map<string, Map<number, KitLayer>>();
  let maxVoices = 32;

  for (const [key, value] of preset.params) {
    if (key === "kit.max_voices") {
      maxVoices = Math.max(1, Math.min(64, Math.round(numeric(value) ?? 32)));
      continue;
    }
    const padMatch = /^pad\.([^.]+)\.([^.]+)$/.exec(key);
    if (padMatch) {
      const [, name, field] = padMatch;
      let pad = pads.get(name);
      if (!pad) {
        pad = defaultPad(name);
        pads.set(name, pad);
        order.push(name);
      }
      applyPadField(pad, field, value);
      continue;
    }
    const layerMatch = /^layer\.([^.]+)\.(\d+)\.([^.]+)$/.exec(key);
    if (layerMatch) {
      const [, name, index, field] = layerMatch;
      let bank = layers.get(name);
      if (!bank) {
        bank = new Map();
        layers.set(name, bank);
      }
      const slot = Number(index);
      let layer = bank.get(slot);
      if (!layer) {
        layer = { low: 1, high: 127, sample: "", gainDb: 0 };
        bank.set(slot, layer);
      }
      if (field === "low") layer.low = Math.round(numeric(value) ?? 1);
      if (field === "high") layer.high = Math.round(numeric(value) ?? 127);
      if (field === "sample") layer.sample = text(value) ?? "";
      if (field === "gain") layer.gainDb = numeric(value) ?? 0;
    }
  }

  const kitPads = order.map((name) => {
    const pad = pads.get(name)!;
    const bank = layers.get(name);
    if (bank) {
      pad.layers = [...bank.entries()]
        .sort(([left], [right]) => left - right)
        .map(([, layer]) => layer);
    }
    return pad;
  });
  return { name: preset.name, maxVoices, pads: kitPads };
}

function defaultPad(name: string): KitPad {
  return {
    name,
    note: 36,
    gainDb: 0,
    pan: 0,
    tune: 0,
    cents: 0,
    decay: 0,
    release: 0.005,
    reverse: false,
    oneShot: true,
    choke: 0,
    layers: [],
  };
}

function applyPadField(pad: KitPad, field: string, value: ParamValue): void {
  switch (field) {
    case "note":
      pad.note = Math.round(numeric(value) ?? 36);
      break;
    case "gain":
      pad.gainDb = numeric(value) ?? 0;
      break;
    case "pan":
      pad.pan = numeric(value) ?? 0;
      break;
    case "tune":
      pad.tune = numeric(value) ?? 0;
      break;
    case "cents":
      pad.cents = numeric(value) ?? 0;
      break;
    case "decay":
      pad.decay = numeric(value) ?? 0;
      break;
    case "release":
      pad.release = numeric(value) ?? 0.005;
      break;
    case "reverse":
      pad.reverse = booleanish(value) ?? false;
      break;
    case "one_shot":
      pad.oneShot = booleanish(value) ?? true;
      break;
    case "choke":
      pad.choke = Math.round(numeric(value) ?? 0);
      break;
    default:
      break;
  }
}

function numeric(value: ParamValue | undefined): number | undefined {
  if (!value) return undefined;
  if ("Number" in value) return value.Number;
  if ("Db" in value) return value.Db;
  if ("Percentage" in value) return value.Percentage;
  if ("Frequency" in value) return value.Frequency;
  if ("Cents" in value) return value.Cents;
  if ("Time" in value) return value.Time;
  return undefined;
}

function booleanish(value: ParamValue | undefined): boolean | undefined {
  if (!value) return undefined;
  if ("Bool" in value) return value.Bool;
  if ("Number" in value) return value.Number !== 0;
  if ("String" in value) return value.String.toLowerCase() === "true";
  return undefined;
}

function text(value: ParamValue | undefined): string | undefined {
  return value && "String" in value ? value.String : undefined;
}

function formatNumber(value: number): string {
  return String(Math.round(value * 1e6) / 1e6);
}

/** Params that fully describe one pad and its layers. */
export function padParamUpdates(pad: KitPad): [string, string][] {
  const prefix = `pad.${pad.name}.`;
  const updates: [string, string][] = [
    [`${prefix}note`, formatNumber(pad.note)],
    [`${prefix}gain`, `${formatNumber(pad.gainDb)}dB`],
    [`${prefix}pan`, formatNumber(pad.pan)],
    [`${prefix}tune`, formatNumber(pad.tune)],
    [`${prefix}cents`, formatNumber(pad.cents)],
    [`${prefix}decay`, `${formatNumber(pad.decay)}s`],
    [`${prefix}release`, `${formatNumber(pad.release)}s`],
    [`${prefix}reverse`, String(pad.reverse)],
    [`${prefix}one_shot`, String(pad.oneShot)],
    [`${prefix}choke`, formatNumber(pad.choke)],
  ];
  const layerPrefix = `layer.${pad.name}.`;
  pad.layers.forEach((layer, index) => {
    updates.push([`${layerPrefix}${index}.low`, formatNumber(layer.low)]);
    updates.push([`${layerPrefix}${index}.high`, formatNumber(layer.high)]);
    updates.push([`${layerPrefix}${index}.sample`, layer.sample]);
    updates.push([`${layerPrefix}${index}.gain`, `${formatNumber(layer.gainDb)}dB`]);
  });
  return updates;
}

/** Load `samplePath` into pad `padName`, creating the pad when it is new. */
export function withPadSample(
  kit: Kit,
  padName: string,
  samplePath: string,
): [string, string][] {
  const name = sanitizePadName(padName);
  const pad = kit.pads.find((candidate) => candidate.name === name);
  if (!pad) {
    const created: KitPad = {
      ...defaultPad(name),
      note: nextFreeNote(kit),
      layers: [{ low: 1, high: 127, sample: samplePath, gainDb: 0 }],
    };
    return padParamUpdates(created);
  }
  const updates: [string, string][] = [];
  if (pad.layers.length === 0) {
    updates.push([`layer.${name}.0.low`, "1"]);
    updates.push([`layer.${name}.0.high`, "127"]);
  }
  updates.push([`layer.${name}.0.sample`, samplePath]);
  return updates;
}

export function sanitizePadName(raw: string): string {
  const cleaned = raw
    .trim()
    .replace(/[^A-Za-z0-9_]+/g, "_")
    .replace(/^_+|_+$/g, "");
  const name = cleaned || "pad";
  return /^[A-Za-z_]/.test(name) ? name : `pad_${name}`;
}

export function nextFreeNote(kit: Kit): number {
  const used = new Set(kit.pads.map((pad) => pad.note));
  for (const note of DEFAULT_PAD_NOTES) {
    if (!used.has(note)) return note;
  }
  for (let note = 36; note < 128; note += 1) {
    if (!used.has(note)) return note;
  }
  return 36;
}

export function midiNoteName(note: number): string {
  const names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
  return `${names[((note % 12) + 12) % 12]}${Math.floor(note / 12) - 1}`;
}

export function sampleFileName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** True when `pad` has at least two overlapping layers (round-robin bank). */
export function roundRobinCount(pad: KitPad): number {
  const counts = new Map<string, number>();
  for (const layer of pad.layers) {
    const key = `${layer.low}-${layer.high}`;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return Math.max(1, ...counts.values());
}
