/** Starter sounds appear only for the kinds of content this computer lacks. */
import { describe, expect, it } from "vitest";
import {
  FEW_INSTRUMENTS,
  FEW_PLUGINS,
  FEW_SAMPLES,
  MAX_STARTER_SOUNDS,
  needsPluginPicks,
  needsSoundPicks,
  pickStarterSounds,
  readablePlugins,
} from "../stores/starterSounds";
import type {
  LocalContentKind,
  LocalContentSummary,
  SoundPackContent,
  SoundPackItem,
} from "../stores/pluginApi";

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

function pack(
  id: string,
  content: SoundPackContent,
  overrides: Partial<SoundPackItem> = {},
): SoundPackItem {
  return {
    id,
    name: id,
    publisher: "Publisher",
    homepage: "https://example.test",
    version: "1.0.0",
    description: "",
    content,
    archive: "zip",
    bytes: 1_000_000,
    unpacked_bytes: 2_000_000,
    license: { name: "CC0-1.0", url: "", text: "" },
    installed: false,
    installed_version: null,
    path: null,
    source: "index",
    ...overrides,
  };
}

const INSTRUMENT = pack("bank", "soundfont");
const SFZ = pack("sfz", "sfz");
const SAMPLES = pack("samples", "samples");

describe("starter suggestions", () => {
  it("offers instrument banks and sample packs separately", () => {
    const items = [INSTRUMENT, SFZ, SAMPLES];

    const fewInstruments = pickStarterSounds(summary(kind(0), 0, FEW_SAMPLES), items);
    expect(fewInstruments).toEqual([INSTRUMENT, SFZ]);

    const fewSamples = pickStarterSounds(summary(kind(0), FEW_INSTRUMENTS, 0), items);
    expect(fewSamples).toEqual([SAMPLES]);

    expect(pickStarterSounds(summary(kind(0), FEW_INSTRUMENTS, FEW_SAMPLES), items)).toEqual([]);
  });

  it("offers every kind before the scan answers", () => {
    expect(pickStarterSounds(null, [INSTRUMENT, SFZ, SAMPLES])).toEqual([INSTRUMENT, SFZ, SAMPLES]);
  });

  it("leaves out installed packs, local packs and unknown content", () => {
    const items = [
      pack("installed", "soundfont", { installed: true }),
      pack("local", "soundfont", { source: "local" }),
      pack("unknown", "loops" as unknown as SoundPackContent),
      INSTRUMENT,
    ];
    expect(pickStarterSounds(summary(kind(0), 0, FEW_SAMPLES), items)).toEqual([INSTRUMENT]);
  });

  it("caps the list in catalog order", () => {
    const items = Array.from({ length: MAX_STARTER_SOUNDS + 2 }, (_, index) =>
      pack(`bank-${index}`, "soundfont"),
    );
    const picked = pickStarterSounds(summary(kind(0), 0, FEW_SAMPLES), items);
    expect(picked).toHaveLength(MAX_STARTER_SOUNDS);
    expect(picked.map((item) => item.id)).toEqual(
      items.slice(0, MAX_STARTER_SOUNDS).map((item) => item.id),
    );
  });

  it("does not count unreadable bundles as plugins the user has", () => {
    const plugins = kind(FEW_PLUGINS + 2, 3);
    expect(readablePlugins(summary(plugins, 0, 0))).toBe(FEW_PLUGINS - 1);
    expect(needsPluginPicks(summary(plugins, 0, 0))).toBe(true);
    expect(needsPluginPicks(summary(kind(FEW_PLUGINS), 0, 0))).toBe(false);
  });

  it("asks for sounds only while instruments or samples are few", () => {
    expect(needsSoundPicks(summary(kind(0), 0, FEW_SAMPLES))).toBe(true);
    expect(needsSoundPicks(summary(kind(0), FEW_INSTRUMENTS, 0))).toBe(true);
    expect(needsSoundPicks(summary(kind(0), FEW_INSTRUMENTS, FEW_SAMPLES))).toBe(false);
  });
});
