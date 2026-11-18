/** One first-run recommendation per role, chosen from the host's catalog. */
import { describe, expect, it } from "vitest";
import { PICK_ROLES, pickRecommendations } from "../stores/onboardingPicks";
import type { PluginCatalogEntry } from "../stores/pluginApi";

function entry(id: string, fields: Partial<PluginCatalogEntry> = {}): PluginCatalogEntry {
  return { id, name: id, vendor: "", tags: [], installs: [], installable: false, installed: false, ...fields };
}

const installed = (candidate: PluginCatalogEntry) => candidate.installed === true;
const installable = (candidate: PluginCatalogEntry) => candidate.installable === true;

describe("first-run picks", () => {
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
