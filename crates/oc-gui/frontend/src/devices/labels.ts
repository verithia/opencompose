/**
 * Localized labels for built-in device metadata. The registry supplies names;
 * descriptions remain UI copy so they can be translated.
 */
import { i18n } from "../stores/i18n";
import type { DeviceCategory, DeviceDef } from "../types/devices";
import { canonicalDeviceId } from "./catalog";

type TranslationKey = Parameters<typeof i18n.t>[0];

const CATEGORY_KEYS: Record<DeviceCategory, TranslationKey> = {
  eq: "device.category.eq",
  dynamics: "device.category.dynamics",
  time: "device.category.time",
  color: "device.category.color",
  utility: "device.category.utility",
};

const DESCRIPTION_KEYS: Record<string, TranslationKey> = {
  gain: "device.desc.gain",
  eq: "device.desc.eq",
  compression: "device.desc.compression",
  "de-esser": "device.desc.deesser",
  multiband: "device.desc.multiband",
  leveler: "device.desc.leveler",
  opto: "device.desc.opto",
  fet: "device.desc.fet",
  iron: "device.desc.iron",
  "passive-eq": "device.desc.passive_eq",
  doubler: "device.desc.doubler",
  exciter: "device.desc.exciter",
  "ducked-send": "device.desc.ducked_send",
  limiter: "device.desc.limiter",
  gate: "device.desc.gate",
  reverb: "device.desc.reverb",
  delay: "device.desc.delay",
  chorus: "device.desc.chorus",
  saturator: "device.desc.saturator",
  distortion: "device.desc.distortion",
  flanger: "device.desc.flanger",
  phaser: "device.desc.phaser",
  stereo_width: "device.desc.stereo_width",
};

export function categoryLabel(category: DeviceCategory): string {
  return i18n.t(CATEGORY_KEYS[category]);
}

export function deviceDescription(device: DeviceDef): string {
  if (device.description) return device.description;
  const key = DESCRIPTION_KEYS[canonicalDeviceId(device.id)];
  return key ? i18n.t(key) : "";
}

const FORMAT_LABELS: Record<string, string> = {
  Vst3: "VST3",
  Vst2: "VST2",
  Clap: "CLAP",
  Au: "AU",
  Lv2: "LV2",
  Internal: "Built-in",
};

/** Human-facing name for a scanned plugin's format. */
export function pluginFormatLabel(format: string): string {
  return FORMAT_LABELS[format] ?? format;
}
