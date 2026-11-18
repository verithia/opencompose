/**
 * Free sound sources the first-run guide suggests when this computer has few
 * instruments or samples. Each one is downloaded from its publisher's page:
 * the archives are not pinned by hash, so the app never fetches them itself.
 */
import type { LocalContentSummary } from "./pluginApi";

export type StarterKind = "soundfont" | "sfz" | "samples";

export interface StarterSound {
  id: string;
  name: string;
  publisher: string;
  kind: StarterKind;
  /** SPDX identifier, or the publisher's own license name. */
  license: string;
  /** Download size from the publisher, when it states one. */
  bytes?: number;
  url: string;
  description: string;
}

export const STARTER_SOUNDS: readonly StarterSound[] = [
  {
    id: "christian-collins.generaluser-gs",
    name: "GeneralUser GS",
    publisher: "S. Christian Collins",
    kind: "soundfont",
    license: "GeneralUser GS License v2.0",
    url: "https://www.schristiancollins.com/generaluser",
    description: "General MIDI bank with 261 instruments and 13 drum kits.",
  },
  {
    id: "freepats.upright-piano-small",
    name: "Upright Piano KW",
    publisher: "FreePats",
    kind: "soundfont",
    license: "CC0-1.0",
    bytes: 6_093_984,
    url: "https://freepats.zenvoid.org/Piano/acoustic-grand-piano.html",
    description: "Compact upright piano.",
  },
  {
    id: "freepats.muldjordkit",
    name: "MuldjordKit",
    publisher: "FreePats",
    kind: "soundfont",
    license: "CC-BY-4.0",
    bytes: 54_877_940,
    url: "https://freepats.zenvoid.org/Percussion/acoustic-drum-kit.html",
    description: "Velocity-layered acoustic drum kit.",
  },
  {
    id: "alexander-holm.salamander-grand",
    name: "Salamander Grand Piano",
    publisher: "Alexander Holm",
    kind: "sfz",
    license: "CC-BY-3.0",
    url: "https://github.com/sfzinstruments/SalamanderGrandPiano",
    description: "Grand piano with 16 velocity layers.",
  },
  {
    id: "versilian.vsco2-ce",
    name: "VSCO 2 Community Edition",
    publisher: "Versilian Studios",
    kind: "sfz",
    license: "CC0-1.0",
    url: "https://github.com/sgossner/VSCO-2-CE",
    description: "Orchestral strings, winds, brass and percussion.",
  },
  {
    id: "99sounds.drum-samples",
    name: "99 Drum Samples I and II",
    publisher: "99Sounds",
    kind: "samples",
    license: "99Sounds License",
    url: "https://99sounds.org/drum-samples/",
    description: "Drum one-shots in 24-bit WAV.",
  },
  {
    id: "producer-space.cc0-library",
    name: "Producer Space CC0 Library",
    publisher: "Producer Space",
    kind: "samples",
    license: "CC0-1.0",
    url: "https://archive.org/details/producer-space-cc0-sample-library",
    description: "Vocal, spoken-word and effect samples in 18 packs.",
  },
];

/** Below these counts the guide suggests more; above them it stays quiet. */
export const FEW_PLUGINS = 10;
export const FEW_INSTRUMENTS = 3;
export const FEW_SAMPLES = 100;

/** Plugins the scan could read, not counting bundles it could not. */
export function readablePlugins(summary: LocalContentSummary): number {
  const unreadable = summary.plugins.items.filter((item) => !item.readable).length;
  return Math.max(0, summary.plugins.total - unreadable);
}

export function needsPluginPicks(summary: LocalContentSummary): boolean {
  return readablePlugins(summary) < FEW_PLUGINS;
}

/**
 * Starter sources to show: instrument banks when instruments are few, sample
 * packs when samples are few, nothing when both are plentiful.
 */
export function pickStarterSounds(summary: LocalContentSummary): StarterSound[] {
  const fewInstruments = summary.instruments.total < FEW_INSTRUMENTS;
  const fewSamples = summary.samples.total < FEW_SAMPLES;
  return STARTER_SOUNDS.filter((sound) => (sound.kind === "samples" ? fewSamples : fewInstruments));
}
