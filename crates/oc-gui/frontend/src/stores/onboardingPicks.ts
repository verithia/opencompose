/**
 * First-run recommendations. Each role names catalog entries in order of
 * preference; the guide shows one entry per role, so a platform where the
 * first choice has no verified build still gets a synth, a reverb and so on.
 *
 * The embedded file holds only ids, never names, addresses or sizes: every
 * install instruction comes from the signed index at runtime.
 */
import embeddedRecommendations from "../data/recommendations.json";
import type { PluginCatalogEntry } from "./pluginApi";

export type PickRole = "synth" | "sampler" | "reverb" | "character" | "mixing";

const ROLE_ORDER: readonly PickRole[] = [
  "synth",
  "sampler",
  "reverb",
  "character",
  "mixing",
];

export interface RecommendationRole {
  role: PickRole;
  ids: readonly string[];
}

export interface Recommendations {
  schema: number;
  roles: readonly RecommendationRole[];
  sounds: readonly string[];
}

function isPickRole(value: unknown): value is PickRole {
  return typeof value === "string" && (ROLE_ORDER as readonly string[]).includes(value);
}

function idList(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((item): item is string => typeof item === "string" && item.trim() !== "")
    .map((item) => item.trim());
}

/** Read the embedded file, dropping any role or id this build does not know. */
export function readRecommendations(value: unknown): Recommendations {
  const source =
    value && typeof value === "object" ? (value as Record<string, unknown>) : {};
  const plugins = Array.isArray(source.plugins) ? source.plugins : [];
  const roles: RecommendationRole[] = [];
  for (const item of plugins) {
    if (!item || typeof item !== "object") continue;
    const row = item as Record<string, unknown>;
    if (!isPickRole(row.role)) continue;
    const ids = idList(row.ids);
    if (ids.length > 0) roles.push({ role: row.role, ids });
  }
  return {
    schema: typeof source.schema === "number" ? source.schema : 0,
    roles,
    sounds: idList(source.sounds),
  };
}

const embedded = readRecommendations(embeddedRecommendations);

/** Roles in display order, each with its candidates in order of preference. */
export const PICK_ROLES: readonly RecommendationRole[] = embedded.roles;

/** Starter sound ids in the order the guide offers them. */
export const RECOMMENDED_SOUNDS: readonly string[] = embedded.sounds;

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
  const picked = new Set<string>();
  const picks: Pick[] = [];
  for (const { role, ids } of PICK_ROLES) {
    const candidates = ids
      .map((id) => byId.get(id))
      .filter((entry): entry is PluginCatalogEntry => entry !== undefined)
      .filter((entry) => !picked.has(entry.id));
    const installable = candidates.filter(isInstallable);
    const entry =
      candidates.find(isInstalled) ??
      installable.find((candidate) => candidate.install_mode === "extract") ??
      installable[0];
    if (entry) {
      picked.add(entry.id);
      picks.push({ role, entry });
    }
  }
  return picks;
}
