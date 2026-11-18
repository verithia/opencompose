/**
 * Which free sounds the first-run guide suggests when this computer has few
 * instruments or samples. The packs come from the signed catalog
 * (`sound_packs_list`), which pins each archive by size and SHA-256, so the
 * app downloads, verifies and unpacks them itself. This module holds only the
 * thresholds and the choice; `soundPacks.ts` runs the installs.
 */
import type { LocalContentSummary, SoundPackItem } from "./pluginApi";

/** Below these counts the guide suggests more; above them it stays quiet. */
export const FEW_PLUGINS = 10;
export const FEW_INSTRUMENTS = 3;
export const FEW_SAMPLES = 100;

/** The guide offers at most this many starter packs; the rest stay in the catalog. */
export const MAX_STARTER_SOUNDS = 6;

/** Plugins the scan could read, not counting bundles it could not. */
export function readablePlugins(summary: LocalContentSummary): number {
  const unreadable = summary.plugins.items.filter((item) => !item.readable).length;
  return Math.max(0, summary.plugins.total - unreadable);
}

export function needsPluginPicks(summary: LocalContentSummary): boolean {
  return readablePlugins(summary) < FEW_PLUGINS;
}

const fewInstruments = (summary: LocalContentSummary) => summary.instruments.total < FEW_INSTRUMENTS;
const fewSamples = (summary: LocalContentSummary) => summary.samples.total < FEW_SAMPLES;

/** Whether the guide shows its starter-sounds section at all. */
export function needsSoundPicks(summary: LocalContentSummary): boolean {
  return fewInstruments(summary) || fewSamples(summary);
}

/**
 * Catalog packs to offer, in catalog order: instrument packs (SoundFont, SFZ)
 * when instruments are few, sample packs when samples are few. Installed
 * packs and packs the catalog no longer lists are left out, and so is any
 * content kind this build does not know. Before the scan answers (`null`)
 * both kinds are offered.
 */
export function pickStarterSounds(
  summary: LocalContentSummary | null,
  items: readonly SoundPackItem[],
): SoundPackItem[] {
  const instruments = summary === null || fewInstruments(summary);
  const samples = summary === null || fewSamples(summary);
  return items
    .filter((item) => item.source === "index" && !item.installed)
    .filter((item) =>
      item.content === "samples"
        ? samples
        : item.content === "soundfont" || item.content === "sfz"
          ? instruments
          : false,
    )
    .slice(0, MAX_STARTER_SOUNDS);
}
