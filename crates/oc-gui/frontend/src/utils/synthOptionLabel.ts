import { i18n } from "../stores/i18n";
import { synthCopyEn, type SynthCopyKey } from "../stores/locales/synthCopy";

const optionKeys = new Map(Object.entries(synthCopyEn).map(([key, label]) => [
  label.toLowerCase().replaceAll("-", " "), key as SynthCopyKey,
]));

/** Translate the name shown in the editor; catalog IDs and option order never change. */
export function synthOptionLabel(label: string): string {
  if (i18n.locale() === "en") return label;
  const suffix = / \((no effect|partial)\)$/.exec(label);
  if (suffix) {
    return i18n.t(suffix[1] === "no effect" ? "synth.ui.optionNoEffect" : "synth.ui.optionPartial", {
      option: synthOptionLabel(label.slice(0, suffix.index)),
    });
  }
  const rhythmic = /^(.*) (dotted|triplet)$/.exec(label);
  if (rhythmic) return i18n.t(rhythmic[2] === "dotted" ? "synth.ui.dotted" : "synth.ui.triplet", { division: synthOptionLabel(rhythmic[1]) });
  const bars = /^(\d+) bars?$/.exec(label);
  if (bars) return i18n.t(Number(bars[1]) === 1 ? "synth.ui.bar" : "synth.ui.bars", { n: bars[1] });
  const macro = /^Macro (\d+)$/.exec(label);
  if (macro) return i18n.t("synth.learn.macroOption", { n: macro[1] });
  const envelope = /^Env (\d+)$/.exec(label);
  if (envelope) return i18n.t("synth.ui.env", { n: envelope[1] });
  if (label === "Amp Env") return i18n.t("synth.ui.ampEnv");
  if (label === "Filter Env") return i18n.t("synth.ui.filterEnv");
  const filter = /^Filter (2 )?(Cutoff|Resonance|Drive)$/.exec(label);
  if (filter) {
    const name = i18n.t(filter[1] ? "synth.ui.secondFilter" : "synth.ui.filter");
    const key = filter[2] === "Cutoff" ? "cutoff" : filter[2] === "Resonance" ? "resonance" : "drive";
    return `${name} · ${i18n.t(`synth.param.${key}`)}`;
  }
  if (label === "Resonance") return i18n.t("synth.param.resonance");
  if (label === "Ring Mod Level") return i18n.t("synth.param.ring");
  const osc = /^Oscillator ([ABC]) (Tune|Level)$/.exec(label);
  if (osc) return `${i18n.t("synth.ui.oscillator", { osc: osc[1] })} · ${i18n.t(osc[2] === "Tune" ? "synth.param.semitones" : "synth.param.level")}`;
  const oscillatorLabels = {
    "Pulse Width": "synth.param.pulse_width",
    "Wavetable Position": "synth.param.wavetable_position",
    "Warp Amount": "synth.param.warp_amount",
    "Warp 2 Amount": "synth.param.warp2_amount",
    "Source Density": "synth.ui.sourceDensity",
    "Source Position": "synth.ui.sourcePosition",
    "Source Morph": "synth.ui.sourceMorph",
    "Source Bend": "synth.ui.sourceBend",
  } as const;
  const target = /^(.+) ([ABC])$/.exec(label);
  if (target && target[1] in oscillatorLabels) {
    return `${i18n.t(oscillatorLabels[target[1] as keyof typeof oscillatorLabels])} ${target[2]}`;
  }
  const key = optionKeys.get(label.toLowerCase().replaceAll("-", " "));
  return key ? i18n.t(key) : label;
}
