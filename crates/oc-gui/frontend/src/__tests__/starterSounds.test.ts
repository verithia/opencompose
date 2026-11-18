/** Starter sounds appear only for the kinds of content this computer lacks. */
import { describe, expect, it } from "vitest";
import {
  FEW_INSTRUMENTS,
  FEW_PLUGINS,
  FEW_SAMPLES,
  STARTER_SOUNDS,
  needsPluginPicks,
  pickStarterSounds,
  readablePlugins,
} from "../stores/starterSounds";
import type { LocalContentKind, LocalContentSummary } from "../stores/pluginApi";

function kind(total: number, unreadable = 0): LocalContentKind {
  const items = Array.from({ length: unreadable }, (_, index) => ({
    name: `Broken ${index}`,
    detail: "",
    format: "vst3",
    path: `/p/${index}.vst3`,
    readable: false,
  }));
  return { total, formats: [], folders: [], items, truncated: false };
}

function summary(plugins: LocalContentKind, instruments: number, samples: number): LocalContentSummary {
  return { plugins, instruments: kind(instruments), samples: kind(samples), scanning: false };
}

describe("starter suggestions", () => {
  it("offers instrument banks and sample packs separately", () => {
    const fewInstruments = pickStarterSounds(summary(kind(0), 0, FEW_SAMPLES));
    expect(fewInstruments.length).toBeGreaterThan(0);
    expect(fewInstruments.every((sound) => sound.kind !== "samples")).toBe(true);

    const fewSamples = pickStarterSounds(summary(kind(0), FEW_INSTRUMENTS, 0));
    expect(fewSamples.length).toBeGreaterThan(0);
    expect(fewSamples.every((sound) => sound.kind === "samples")).toBe(true);

    expect(pickStarterSounds(summary(kind(0), FEW_INSTRUMENTS, FEW_SAMPLES))).toEqual([]);
  });

  it("does not count unreadable bundles as plugins the user has", () => {
    const plugins = kind(FEW_PLUGINS + 2, 3);
    expect(readablePlugins(summary(plugins, 0, 0))).toBe(FEW_PLUGINS - 1);
    expect(needsPluginPicks(summary(plugins, 0, 0))).toBe(true);
    expect(needsPluginPicks(summary(kind(FEW_PLUGINS), 0, 0))).toBe(false);
  });

  it("links every starter to an https page with a license", () => {
    const ids = new Set<string>();
    for (const sound of STARTER_SOUNDS) {
      expect(sound.url.startsWith("https://")).toBe(true);
      expect(sound.license).not.toBe("");
      expect(ids.has(sound.id)).toBe(false);
      ids.add(sound.id);
    }
  });
});
