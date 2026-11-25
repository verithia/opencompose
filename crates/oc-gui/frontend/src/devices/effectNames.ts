/**
 * Built-in effect identity and its display name.
 *
 * The rack picks a glyph by the same list, and edit history names an effect
 * token with the same label, so one table owns both.
 */
import type { EffectType } from "../types/project";
import { DEVICE_CATALOG } from "../data/deviceCatalog.gen";

/** Every built-in effect family: the string members of EffectType. */
export type BuiltinEffectType = Exclude<EffectType, { Plugin: string }>;

/** Ordered built-in families; the glyph table must cover every one. */
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
  "StereoMatrix",
  "FrequencyShift",
  "Vocoder",
  "Gain",
  "AllPass",
  "TransferShaper",
] as const satisfies readonly BuiltinEffectType[];

export const EFFECT_LABELS: Record<BuiltinEffectType, string> = {
  Eq: "EQ",
  DynamicEq: "Dynamic EQ",
  Compression: "Compressor",
  Limiter: "Limiter",
  Gate: "Gate",
  DeEsser: "De-esser",
  BreathSplit: "Breath Split",
  Multiband: "Multiband",
  Leveler: "Leveler",
  Opto: "Opto Compressor",
  Fet: "FET Compressor",
  Iron: "Iron",
  PassiveEq: "Passive EQ",
  Doubler: "Doubler",
  Exciter: "Air Exciter",
  DuckedSend: "Ducked Send",
  Resonance: "Resonance Suppressor",
  Tuner: "Tuner",
  Reverb: "Reverb",
  ReverbFdn: "FDN Reverb",
  Delay: "Delay",
  Distortion: "Distortion",
  Saturator: "Saturator",
  Chorus: "Chorus",
  Flanger: "Flanger",
  Phaser: "Phaser",
  StereoWidth: "Stereo width",
  StereoMatrix: "Stereo Matrix",
  FrequencyShift: "Frequency Shifter",
  Vocoder: DEVICE_CATALOG.devices.find((device) => device.id === "vocoder")?.name ?? "vocoder",
  Gain: "Gain",
  AllPass: "All-pass",
  TransferShaper: "Transfer Shaper",
};

/** A built-in effect's display label; a plugin keeps its own id. */
export function effectLabel(type: EffectType): string {
  if (typeof type !== "string") return type.Plugin;
  return EFFECT_LABELS[type as BuiltinEffectType] ?? type;
}
