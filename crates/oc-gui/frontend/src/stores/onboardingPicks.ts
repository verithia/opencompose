/**
 * First-run recommendations. Each role names catalog entries in order of
 * preference; the guide shows one entry per role, so a platform where the
 * first choice has no verified build still gets a synth, a reverb and so on.
 */
import type { PluginCatalogEntry } from "./pluginApi";

export type PickRole = "synth" | "sampler" | "reverb" | "character" | "mixing" | "tools";

/** Roles in display order, each with its candidates in order of preference. */
export const PICK_ROLES: readonly { role: PickRole; ids: readonly string[] }[] = [
  {
    role: "synth",
    ids: [
      "surge-synth-team.surge-xt",
      "asb2m10.dexed",
      "u-he.tyrelln6",
      "u-he.podolski",
      "discodsp.ob-xd",
    ],
  },
  { role: "sampler", ids: ["sfztools.sfizz", "decent-samples.decent-sampler"] },
  {
    role: "reverb",
    ids: [
      "valhalla-dsp.supermassive",
      "dragonfly-reverb.dragonfly-reverb",
      "u-he.protoverb",
      "voxengo.oldskoolverb",
    ],
  },
  {
    role: "character",
    ids: ["chowdsp.chowtape-model", "airwindows.consolidated", "wolf-plugins.wolf-shaper"],
  },
  {
    role: "mixing",
    ids: ["tokyo-dawn.tdr-nova", "voxengo.span", "lsp-plugins.lsp-plugins"],
  },
  // Optional index entries: they appear only when the signed catalog
  // publishes a managed runtime for them.
  { role: "tools", ids: ["spleeter", "ffmpeg"] },
];

export interface Pick {
  role: PickRole;
  entry: PluginCatalogEntry;
}

/**
 * One entry per role: one the user already has, else one that installs
 * without a vendor installer, else any installable one. A role with none of
 * these is left out.
 */
export function pickRecommendations(
  entries: readonly PluginCatalogEntry[],
  isInstalled: (entry: PluginCatalogEntry) => boolean,
  isInstallable: (entry: PluginCatalogEntry) => boolean,
): Pick[] {
  const byId = new Map(entries.map((entry) => [entry.id, entry]));
  const picks: Pick[] = [];
  for (const { role, ids } of PICK_ROLES) {
    const candidates = ids
      .map((id) => byId.get(id))
      .filter((entry): entry is PluginCatalogEntry => entry !== undefined);
    const installable = candidates.filter(isInstallable);
    const entry =
      candidates.find(isInstalled) ??
      installable.find((candidate) => candidate.install_mode === "extract") ??
      installable[0];
    if (entry) picks.push({ role, entry });
  }
  return picks;
}
