/** One first-run recommendation per role, chosen from the host's catalog. */
import { readFileSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import recommendations from "../data/recommendations.json";
import {
  PICK_ROLES,
  RECOMMENDED_SOUNDS,
  pickRecommendations,
  readRecommendations,
} from "../stores/onboardingPicks";
import type { PluginCatalogEntry } from "../stores/pluginApi";

function entry(id: string, fields: Partial<PluginCatalogEntry> = {}): PluginCatalogEntry {
  return { id, name: id, vendor: "", tags: [], installs: [], installable: false, installed: false, ...fields };
}

const installed = (candidate: PluginCatalogEntry) => candidate.installed === true;
const installable = (candidate: PluginCatalogEntry) => candidate.installable === true;

interface PublishedInstall {
  kind: string;
  platform: string;
  format: string;
  archive?: string;
  url: string;
  sha256?: string;
  runtime?: unknown;
  wrapped?: boolean;
}

interface PublishedCatalog {
  entries: { id: string; installs: PublishedInstall[] }[];
  packs: { id: string }[];
}

const published = JSON.parse(
  readFileSync(path.resolve(process.cwd(), "../../../catalog/plugin-catalog.json"), "utf8"),
) as PublishedCatalog;

const PLATFORMS = ["macos-aarch64", "macos-x86_64", "windows-x86_64", "linux-x86_64"];

/** Archive spelling the backend infers from a download name. */
function inferArchive(url: string): string | undefined {
  const lower = url.toLowerCase();
  const file = lower.split(/[?#]/, 1)[0];
  if (file.endsWith(".tar.gz") || file.endsWith(".tgz")) return "tar-gz";
  if (file.endsWith(".tar.xz")) return "tar-xz";
  if (file.endsWith(".7z")) return "7z";
  if (file.endsWith(".zip")) return "zip";
  if (file.endsWith(".dmg")) return "dmg";
  if (file.endsWith(".pkg")) return "pkg";
  if ([".exe", ".msi", ".appimage", ".deb", ".rpm"].some((suffix) => file.endsWith(suffix))) {
    return "installer";
  }
  return undefined;
}

const FORMAT_RANK: Record<string, number> = {
  vst3: 0,
  clap: 1,
  vst2: 2,
  lv2: 3,
  au: 4,
  standalone: 5,
};

/**
 * Mirror `oc_market::plan` for a published catalog: the best-ranked build must
 * be a pinned, non-page artifact with an archive the installer recognises.
 */
function backendInstallable(entry: PublishedCatalog["entries"][number], platform: string): boolean {
  const candidates = entry.installs.filter((install) => install.platform === platform);
  if (candidates.length === 0) return false;
  const rank = (install: PublishedInstall) =>
    (install.kind === "vendor_page" ? 10 : 0) + (FORMAT_RANK[install.format] ?? 6);
  const chosen = [...candidates].sort((left, right) => rank(left) - rank(right))[0];
  if (chosen.kind === "vendor_page") return false;
  if (!chosen.sha256) return false;
  if (chosen.runtime || chosen.wrapped) return true;
  return (chosen.archive ?? inferArchive(chosen.url)) !== undefined;
}

describe("first-run picks", () => {
  it("takes its roles, candidates and sounds from the embedded list", () => {
    const embedded = readRecommendations(recommendations);
    expect(PICK_ROLES).toEqual(embedded.roles);
    expect(RECOMMENDED_SOUNDS).toEqual(recommendations.sounds);

    const entries = embedded.roles.map(({ ids }) => entry(ids[0], { installable: true }));
    const picks = pickRecommendations([...entries].reverse(), installed, installable);
    expect(picks.map((pick) => [pick.role, pick.entry.id])).toEqual(
      embedded.roles.map(({ role, ids }) => [role, ids[0]]),
    );
  });

  it("embeds ids only, without names, addresses or sizes", () => {
    expect(Object.keys(recommendations).sort()).toEqual(["plugins", "schema", "sounds"]);
    expect(recommendations.schema).toBe(1);
    for (const row of recommendations.plugins) {
      expect(Object.keys(row).sort()).toEqual(["ids", "role"]);
      expect(row.ids.every((id) => typeof id === "string" && id.length > 0)).toBe(true);
    }
    expect(recommendations.sounds.every((id) => typeof id === "string" && id.length > 0)).toBe(true);
  });

  it("names only entries and packs the published catalog still offers", () => {
    const byId = new Map(published.entries.map((entry) => [entry.id, entry]));
    for (const { role, ids } of PICK_ROLES) {
      for (const id of ids) {
        expect(byId.has(id), `${role} names ${id}`).toBe(true);
      }
    }
    const packs = new Set(published.packs.map((pack) => pack.id));
    for (const id of RECOMMENDED_SOUNDS) {
      expect(packs.has(id), `sounds names ${id}`).toBe(true);
    }
  });

  it("offers a pinned, non-page build for every role on every platform", () => {
    const byId = new Map(published.entries.map((entry) => [entry.id, entry]));
    for (const platform of PLATFORMS) {
      const entries = published.entries.map(
        (entry) =>
          ({
            ...entry,
            installable: backendInstallable(entry, platform),
          }) as unknown as PluginCatalogEntry,
      );
      const picks = pickRecommendations(entries, () => false, (candidate) => candidate.installable === true);
      for (const { role } of PICK_ROLES) {
        const pick = picks.find((candidate) => candidate.role === role);
        expect(pick, `${role} has an installable pick on ${platform}`).toBeDefined();
        expect(
          backendInstallable(byId.get(pick!.entry.id)!, platform),
          `${role} installs on ${platform} from ${pick!.entry.id}`,
        ).toBe(true);
      }
    }
  });

  it("ignores unknown roles and malformed ids at load", () => {
    const parsed = readRecommendations({
      schema: 1,
      plugins: [
        { role: "future", ids: ["unknown.role"] },
        { role: "synth", ids: ["surge-synth-team.surge-xt", "", 7] },
        { role: "mixing", ids: "not-a-list" },
      ],
      sounds: ["christian-collins.generaluser-gs", null, ""],
    });
    expect(parsed.roles).toEqual([
      { role: "synth", ids: ["surge-synth-team.surge-xt"] },
    ]);
    expect(parsed.sounds).toEqual(["christian-collins.generaluser-gs"]);
  });

  it("falls back to the next candidate when the first has no verified build", () => {
    const picks = pickRecommendations(
      [
        entry("surge-synth-team.surge-xt"),
        entry("asb2m10.dexed", { installable: true, install_mode: "extract" }),
      ],
      installed,
      installable,
    );
    expect(picks.map((pick) => [pick.role, pick.entry.id])).toEqual([["synth", "asb2m10.dexed"]]);
  });

  it("prefers an entry the user has, then a headless install, then a vendor installer", () => {
    const reverbs = [
      entry("valhalla-dsp.supermassive", { installable: true, install_mode: "installer" }),
      entry("dragonfly-reverb.dragonfly-reverb", { installable: true, install_mode: "extract" }),
      entry("u-he.protoverb", { installed: true }),
    ];
    expect(pickRecommendations(reverbs, installed, installable)[0].entry.id).toBe("u-he.protoverb");
    expect(pickRecommendations(reverbs.slice(0, 2), installed, installable)[0].entry.id).toBe(
      "dragonfly-reverb.dragonfly-reverb",
    );
    expect(pickRecommendations(reverbs.slice(0, 1), installed, installable)[0].entry.id).toBe(
      "valhalla-dsp.supermassive",
    );
  });

  it("lists roles in display order and leaves out roles with nothing to offer", () => {
    const everyFirst = PICK_ROLES.map(({ ids }) => entry(ids[0], { installable: true }));
    const picks = pickRecommendations([...everyFirst].reverse(), installed, installable);
    expect(picks.map((pick) => pick.role)).toEqual(PICK_ROLES.map(({ role }) => role));
    expect(pickRecommendations([entry("unrelated", { installable: true })], installed, installable)).toEqual([]);
  });

  it("does not show one entry for several roles", () => {
    const picks = pickRecommendations(
      [
        entry("airwindows.consolidated", { installable: true, install_mode: "extract" }),
        entry("voxengo.boogex", { installable: true, install_mode: "installer" }),
        entry("lsp-plugins.lsp-plugins", { installable: true, install_mode: "extract" }),
      ],
      installed,
      installable,
    );
    expect(picks.map((pick) => [pick.role, pick.entry.id])).toEqual([
      ["reverb", "airwindows.consolidated"],
      ["character", "voxengo.boogex"],
      ["mixing", "lsp-plugins.lsp-plugins"],
    ]);
  });

  it("skips an installed entry an earlier role already took", () => {
    const picks = pickRecommendations(
      [
        entry("airwindows.consolidated", { installed: true, installable: true, install_mode: "extract" }),
        entry("voxengo.boogex", { installable: true, install_mode: "extract" }),
      ],
      installed,
      installable,
    );
    expect(picks.map((pick) => [pick.role, pick.entry.id])).toEqual([
      ["reverb", "airwindows.consolidated"],
      ["character", "voxengo.boogex"],
    ]);
  });
});
