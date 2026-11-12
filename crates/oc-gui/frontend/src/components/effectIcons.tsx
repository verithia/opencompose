/**
 * Effect glyphs — tiny line icons, one per built-in device family, drawn in a
 * 12×12 box with `currentColor` strokes so they take the theme colour.
 *
 * Each glyph shows what the device does to the signal rather than a symbol:
 * an EQ curve, a compressor's knee, a reverb's decaying tail, repeating delay
 * taps, a clipped wave, and so on.
 */
import type { JSX } from "solid-js";
import type { EffectType } from "../types/project";

type Glyph = (props: { x: number; y: number; size: number }) => JSX.Element;

interface GlyphEntry {
  glyph: Glyph;
  label: string;
}

/** Every built-in effect family: the string members of EffectType. */
export type BuiltinEffectType = Exclude<EffectType, { Plugin: string }>;

const path = (d: string): Glyph => (props) => (
  <path
    d={d}
    transform={`translate(${props.x} ${props.y}) scale(${props.size / 12})`}
    fill="none"
    stroke="currentColor"
    stroke-width={1.3}
    stroke-linecap="round"
    stroke-linejoin="round"
    vector-effect="non-scaling-stroke"
  />
);

/** Ordered built-in families; the glyph table below must cover every one. */
export const BUILTIN_EFFECT_TYPES = [
  "Eq",
  "DynamicEq",
  "Compression",
  "DeEsser",
  "BreathSplit",
  "Multiband",
  "Leveler",
  "Opto",
  "Fet",
  "Iron",
  "PassiveEq",
  "Doubler",
  "Exciter",
  "DuckedSend",
  "Resonance",
  "Tuner",
  "Reverb",
  "ReverbFdn",
  "Delay",
  "Distortion",
  "Chorus",
  "Flanger",
  "Phaser",
  "Limiter",
  "Gate",
  "Saturator",
  "StereoWidth",
  "Gain",
] as const satisfies readonly BuiltinEffectType[];

type ListedEffectType = (typeof BUILTIN_EFFECT_TYPES)[number];

// Compile error when a new EffectType member is not in the list above.
const missingEffectTypes: Record<Exclude<BuiltinEffectType, ListedEffectType>, never> = {};
void missingEffectTypes;

const PLUGIN_GLYPH: GlyphEntry = {
  label: "Plugin",
  glyph: path("M4 1.5 V4 M8 1.5 V4 M2.5 4 H9.5 V6 C9.5 8 8 9 6 9 C4 9 2.5 8 2.5 6 Z M6 9 V10.5"),
};

const GLYPHS: Record<ListedEffectType, GlyphEntry> = {
  // Bell curve on a flat line
  Eq: { label: "EQ", glyph: path("M1 8 H3.5 C5 8 5 3 6 3 C7 3 7 8 8.5 8 H11") },
  // Bell plus a dotted "moving" arrow
  DynamicEq: { label: "Dynamic EQ", glyph: path("M1 8 H3.5 C5 8 5 4 6 4 C7 4 7 8 8.5 8 H11 M6 1.5 V2.5 M6 9.5 V10.5") },
  // Transfer curve with a knee
  Compression: { label: "Compressor", glyph: path("M1.5 10.5 L6 6 L10.5 4.5") },
  // Knee + flat ceiling
  Limiter: { label: "Limiter", glyph: path("M1.5 10.5 L6.5 5.5 H10.5 M1.5 3.5 H10.5") },
  // Gate: closed then open
  Gate: { label: "Gate", glyph: path("M1 9 H4 V3 H8 V9 H11") },
  // Sibilance "s" over a notch
  DeEsser: { label: "De-esser", glyph: path("M8 3 C6 2 4 3 5 4.8 C6 6.3 8 6.2 7 8 C6.2 9.4 4 9 3.5 8.5") },
  // One stream forking into breath and voice
  BreathSplit: { label: "Breath Split", glyph: path("M1.2 6 H2.6 C4.8 6 4.8 1.8 7.4 1.8 H10.4 M2.6 6 C4.8 6 4.8 10.2 7.4 10.2 H10.4") },
  // Three stacked bands
  Multiband: { label: "Multiband", glyph: path("M1 3 H11 M1 6 H11 M1 9 H11 M4 2 V4 M8 5 V7 M5.5 8 V10") },
  // Slow level ride over the signal
  Leveler: { label: "Leveler", glyph: path("M1 9.5 C2.6 9.5 3 5 4.8 5 C6.6 5 7 9.5 8.6 9.5 H11 M1.5 2.5 H8.5 M7 1.3 L8.7 2.5 L7 3.7") },
  // Optical cell plus the slow recovery curve
  Opto: { label: "Opto Compressor", glyph: path("M1 9.5 H2.4 C3.2 9.5 3.5 5 5 5 C6.5 5 6.8 9.5 7.6 9.5 H11 M9.1 2.1 L10.1 3.1 L9.1 4.1 L8.1 3.1 Z") },
  // Hard knee with a fast attack
  Fet: { label: "FET Compressor", glyph: path("M1.5 10.5 H3.4 L6.4 6.4 H10.5 M8 1.5 V3.6 M7 2.5 L8 3.7 L9 2.5") },
  // Asymmetric saturation with core laminations
  Iron: { label: "Iron", glyph: path("M1 9.2 H2.8 C4 9.2 4 4.8 6 4.8 C8 4.8 8 9.2 9.2 9.2 H11 M5.2 1.6 V3 M6.8 1.6 V3") },
  // Shelf
  PassiveEq: { label: "Passive EQ", glyph: path("M1 8.6 H3.2 C4.8 8.6 4.8 4.4 6.4 4.4 H11") },
  // Two identical waves offset in time
  Doubler: { label: "Doubler", glyph: path("M1 6 C2 3.6 3.4 3.6 4.4 6 C5.4 8.4 6.8 8.4 7.8 6 M4 6 C5 3.6 6.4 3.6 7.4 6 C8.4 8.4 9.8 8.4 10.8 6") },
  // Fundamental with added harmonics above it
  Exciter: { label: "Air Exciter", glyph: path("M1 8.6 C2 6.6 2.6 6.6 3.6 8.6 C4.6 10.6 5.2 10.6 6.2 8.6 C7.2 6.6 7.8 6.6 8.8 8.6 C9.6 10.2 10.2 10.4 10.8 8.6 M3.2 3.2 L4 1.6 L4.8 3.2 L5.6 1.6 L6.4 3.2 L7.2 1.6 L8 3.2") },
  // Send envelope ducked by its key
  DuckedSend: { label: "Ducked Send", glyph: path("M1 4 H3.8 L6 9.2 L8.2 4 H11 M6 11 V9.8") },
  // Narrow band notched out of a flat response
  Resonance: { label: "Resonance Suppressor", glyph: path("M1 4.2 H4.3 V9.6 H7.7 V4.2 H11 M6 1.3 V2.7 M5.2 2 L6 2.8 L6.8 2") },
  // Tuning fork
  Tuner: { label: "Tuner", glyph: path("M4.2 2 V5.8 C4.2 7.8 7.8 7.8 7.8 5.8 V2 M6 7.2 V10.6") },
  // Decaying tail
  Reverb: { label: "Reverb", glyph: path("M1.5 2 V10 M4 4 V10 M6.5 6 V10 M9 7.5 V10 M11 8.8 V10") },
  // Room box around a decaying tail
  ReverbFdn: { label: "FDN Reverb", glyph: path("M2.2 1.8 H9.8 V10.2 H2.2 Z M4 8.4 V6 M6 8.4 V6.8 M8 8.4 V7.5") },
  // Echo taps
  Delay: { label: "Delay", glyph: path("M1.5 3 V10 M5 5 V10 M8.5 7 V10") },
  // Clipped sine
  Distortion: { label: "Distortion", glyph: path("M1 6 L2.5 2.5 H5 L7 9.5 H9.5 L11 6") },
  // Soft-clipped sine
  Saturator: { label: "Saturator", glyph: path("M1 6 C2 2.5 4 2.5 5 3 C6 3.5 6 8.5 7 9 C8 9.5 10 9.5 11 6") },
  // Two detuned waves
  Chorus: { label: "Chorus", glyph: path("M1 5 C3 2 4 2 6 5 S9 8 11 5 M1 7 C3 4 4 4 6 7 S9 10 11 7") },
  // Comb sweep
  Flanger: { label: "Flanger", glyph: path("M1 9 L3 4 L5 9 L7 4 L9 9 L11 4") },
  // Rotating phase
  Phaser: { label: "Phaser", glyph: path("M6 2 A4 4 0 1 1 2.3 7.5 M2.3 7.5 L1.5 5.5 M2.3 7.5 L4.4 7") },
  // Two arrows apart
  StereoWidth: { label: "Stereo width", glyph: path("M6 3 V9 M1 6 H4 M1 6 L2.5 4.5 M1 6 L2.5 7.5 M11 6 H8 M11 6 L9.5 4.5 M11 6 L9.5 7.5") },
  // Fader
  Gain: { label: "Gain", glyph: path("M6 1.5 V10.5 M3.5 5 H8.5") },
};

/** True when the built-in family has its own glyph (plugins and unknown types do not). */
export function hasEffectGlyph(type: EffectType): boolean {
  return typeof type === "string" && Object.prototype.hasOwnProperty.call(GLYPHS, type);
}

/** Resolve the glyph entry, falling back to the plug for plugins and unknown types. */
export function effectGlyph(type: EffectType): GlyphEntry {
  return hasEffectGlyph(type) ? GLYPHS[type as ListedEffectType] : PLUGIN_GLYPH;
}

export function effectKey(type: EffectType): string {
  return typeof type === "string" ? type : "Plugin";
}

export function effectLabel(type: EffectType): string {
  if (typeof type !== "string") return type.Plugin;
  return hasEffectGlyph(type) ? GLYPHS[type as ListedEffectType].label : type;
}

/** An effect glyph positioned inside an SVG. */
export function EffectGlyph(props: { type: EffectType; x: number; y: number; size?: number; bypassed?: boolean }) {
  return (
    <g class="effect-glyph" data-effect={effectKey(props.type)} opacity={props.bypassed ? 0.4 : 1}>
      <title>{effectLabel(props.type)}</title>
      {effectGlyph(props.type).glyph({ x: props.x, y: props.y, size: props.size ?? 12 })}
    </g>
  );
}

/** Standalone inline icon for HTML contexts (e.g. effect rack slots). */
export function EffectIcon(props: { type: EffectType; size?: number }) {
  const size = () => props.size ?? 12;
  return (
    <svg
      class="effect-icon"
      data-effect={effectKey(props.type)}
      width={size()}
      height={size()}
      viewBox="0 0 12 12"
      aria-hidden="true"
    >
      {effectGlyph(props.type).glyph({ x: 0, y: 0, size: 12 })}
    </svg>
  );
}
