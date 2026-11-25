import { INJECTED_PRESET_PREFIX, instrumentDisplay, type InstrumentPath, type PatternDef, type PresetDef, type Project, type Track } from "../types/project";
import { i18n } from "../stores/i18n";
import { resolvePresetForDisplay } from "./presetInheritance";

export interface EffectivePatternSound {
  origin: "pattern" | "track";
  presetName: string | null;
  preset: PresetDef | null;
  instrument: InstrumentPath;
  engine: string | null;
}

/** Display the source chosen by the source-lane owner: authored pattern presets
 * replace the host preset; injected defaults inherit it. Missing explicit
 * references stay missing rather than claiming that the track fallback plays. */
export function effectivePatternSound(
  project: Pick<Project, "presets">,
  track: Track,
  pattern?: PatternDef | null,
): EffectivePatternSound {
  const own = pattern?.preset && !pattern.preset.startsWith(INJECTED_PRESET_PREFIX)
    ? pattern.preset : null;
  const presetName = own ?? track.preset;
  const authored = project.presets.find((preset) => preset.name === presetName);
  const category = pattern?.instrument.category.trim().toLowerCase();
  const instrument = pattern && category && category !== "unknown" && category !== "audio"
    ? pattern.instrument : track.instrument;
  return {
    origin: own ? "pattern" : "track",
    presetName,
    preset: authored ? resolvePresetForDisplay(authored, project.presets) : null,
    instrument,
    // Source-lane planning retains the hosting track's legacy engine binding.
    engine: track.engine,
  };
}

export function patternSoundLabel(sound: EffectivePatternSound): string {
  if (sound.presetName) {
    return sound.preset ? sound.presetName : `${sound.presetName} (${i18n.t("tooltip.notFound")})`;
  }
  return instrumentDisplay(sound.instrument) || i18n.t("tooltip.noSound");
}

export function patternSoundOrigin(sound: EffectivePatternSound): string {
  return i18n.t(sound.origin === "pattern" ? "clip.sound.patternOrigin" : "clip.sound.trackOrigin");
}
