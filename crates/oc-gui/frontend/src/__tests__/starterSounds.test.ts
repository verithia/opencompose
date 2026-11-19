/** Starter sounds appear only for the kinds of content this computer lacks. */
import { describe, expect, it } from "vitest";
import recommendations from "../data/recommendations.json";
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

const GENERAL_USER = pack("christian-collins.generaluser-gs", "soundfont");
const UPRIGHT = pack("freepats.upright-piano-kw-small", "soundfont");
const MULDJORD = pack("freepats.muldjordkit-sf2", "soundfont");
const NYLON = pack("meta.nylon-guitar-ogg", "sfz");
const SALAMANDER = pack("freepats.salamander-grand-piano-sf2", "soundfont");
const AKWF = pack("adventurekid.akwf", "samples");
const OCULUS = pack("meta.oculus-audio-pack-ogg", "samples");

const RECOMMENDED = [GENERAL_USER, UPRIGHT, MULDJORD, NYLON, SALAMANDER, AKWF, OCULUS];

describe("starter suggestions", () => {
  it("uses the embedded list as its only source of pack ids", () => {
    expect(RECOMMENDED.map((item) => item.id)).toEqual(recommendations.sounds);
  });

  it("offers instrument banks and sample packs separately", () => {
    const fewInstruments = pickStarterSounds(summary(kind(0), 0, FEW_SAMPLES), RECOMMENDED);
    expect(fewInstruments).toEqual([GENERAL_USER, UPRIGHT, MULDJORD, NYLON, SALAMANDER]);

    const fewSamples = pickStarterSounds(summary(kind(0), FEW_INSTRUMENTS, 0), RECOMMENDED);
    expect(fewSamples).toEqual([AKWF, OCULUS]);

    expect(pickStarterSounds(summary(kind(0), FEW_INSTRUMENTS, FEW_SAMPLES), RECOMMENDED)).toEqual([]);
  });

  it("offers every recommended kind before the scan answers", () => {
    expect(pickStarterSounds(null, RECOMMENDED)).toEqual(RECOMMENDED.slice(0, MAX_STARTER_SOUNDS));
  });

  it("follows the embedded order, not the catalog order", () => {
    const reversed = [...RECOMMENDED].reverse();
    expect(pickStarterSounds(null, reversed)).toEqual(RECOMMENDED.slice(0, MAX_STARTER_SOUNDS));
  });

  it("leaves out installed, local, unknown and non-recommended packs", () => {
    const items = [
      pack("some.other.pack", "soundfont"),
      { ...GENERAL_USER, installed: true },
      { ...UPRIGHT, source: "local" as const },
      { ...SALAMANDER, content: "loops" as unknown as SoundPackContent },
      MULDJORD,
      NYLON,
      AKWF,
      OCULUS,
    ];
    expect(pickStarterSounds(null, items)).toEqual([MULDJORD, NYLON, AKWF, OCULUS]);
  });

  it("caps the list in recommendation order", () => {
    const picked = pickStarterSounds(null, RECOMMENDED);
    expect(picked).toHaveLength(MAX_STARTER_SOUNDS);
    expect(picked.map((item) => item.id)).toEqual(
      RECOMMENDED.slice(0, MAX_STARTER_SOUNDS).map((item) => item.id),
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
