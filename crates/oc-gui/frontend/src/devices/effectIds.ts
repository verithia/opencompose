/**
 * Effect type ↔ device id: the one table that names a chain entry.
 *
 * The rack picks a panel by it, and an automation lane resolves the parameter
 * range of the device it drives through the same mapping, so a new effect
 * family is added here once.
 */
import type { EffectType } from "../types/project";
import type { BuiltinEffectType } from "../components/effectIcons";

// Compile error when a new EffectType member has no device id below.
export const DEVICE_IDS = {
  Gain: "gain",
  Eq: "eq",
  DynamicEq: "dynamic-eq",
  Compression: "compression",
  DeEsser: "de-esser",
  BreathSplit: "breath-split",
  Multiband: "multiband",
  Leveler: "leveler",
  Opto: "opto",
  Fet: "fet",
  Iron: "iron",
  PassiveEq: "passive-eq",
  Doubler: "doubler",
  Exciter: "exciter",
  DuckedSend: "ducked-send",
  Resonance: "resonance",
  Tuner: "tuner",
  Reverb: "reverb",
  ReverbFdn: "reverb-fdn",
  Delay: "delay",
  Distortion: "distortion",
  Chorus: "chorus",
  Flanger: "flanger",
  Phaser: "phaser",
  Limiter: "limiter",
  Gate: "gate",
  Saturator: "saturator",
  StereoWidth: "stereo_width",
} as const satisfies Record<BuiltinEffectType, string>;

export type DeviceId = (typeof DEVICE_IDS)[BuiltinEffectType];

/** The device a built-in effect names; null for a hosted plugin. */
export function effectDeviceId(type: EffectType): string | null {
  if (typeof type === "string") return DEVICE_IDS[type as BuiltinEffectType] ?? type.toLowerCase();
  return null;
}

/** The automation lane name an effect type answers to: `de-esser` is `de_esser`. */
export function effectLaneName(type: EffectType): string | null {
  const id = effectDeviceId(type);
  return id === null ? null : id.replaceAll("-", "_");
}
