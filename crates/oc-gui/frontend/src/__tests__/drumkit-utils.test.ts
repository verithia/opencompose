import { describe, expect, it } from "vitest";
import type { HitEvent, ParamValue, PresetDef } from "../types/project";
import {
  DEFAULT_PAD_NOTES,
  drumForNote,
  hitNote,
  hitStartQuarters,
  isDrumKitPreset,
  kitFromPreset,
  kitLaneRows,
  nextFreeNote,
  padParamUpdates,
  roundRobinCount,
  sanitizePadName,
  withPadSample,
} from "../utils/drumkit";

function kitPreset(): PresetDef {
  const params: [string, ParamValue][] = [
    ["kit.max_voices", { Number: 24 }],
    ["pad.kick.note", { Number: 36 }],
    ["pad.kick.gain", { Db: -2 }],
    ["pad.kick.decay", { Time: 0.4 }],
    ["pad.kick.one_shot", { Bool: true }],
    ["pad.kick.choke", { Number: 1 }],
    ["pad.snare.note", { Number: 38 }],
    ["pad.snare.one_shot", { Bool: false }],
    ["layer.kick.0.low", { Number: 1 }],
    ["layer.kick.0.high", { Number: 127 }],
    ["layer.kick.0.sample", { String: "samples/kick.wav" }],
    ["layer.snare.0.low", { Number: 1 }],
    ["layer.snare.0.high", { Number: 63 }],
    ["layer.snare.0.sample", { String: "samples/snare_soft_a.wav" }],
    ["layer.snare.1.low", { Number: 1 }],
    ["layer.snare.1.high", { Number: 63 }],
    ["layer.snare.1.sample", { String: "samples/snare_soft_b.wav" }],
    ["layer.snare.2.low", { Number: 64 }],
    ["layer.snare.2.high", { Number: 127 }],
    ["layer.snare.2.sample", { String: "samples/snare_hard.wav" }],
  ];
  return {
    name: "Basic Kit",
    source: "builtin",
    plugin: "builtin.drumkit",
    program: null,
    patch: null,
    file: null,
    root_note: null,
    extends: null,
    params,
    effects: [],
  };
}

describe("drum-kit document helpers", () => {
  it("lowers preset params into pads and velocity layers", () => {
    const preset = kitPreset();
    expect(isDrumKitPreset(preset)).toBe(true);
    const kit = kitFromPreset(preset);
    expect(kit.maxVoices).toBe(24);
    expect(kit.pads.map((pad) => pad.name)).toEqual(["kick", "snare"]);
    const snare = kit.pads[1];
    expect(snare.layers.map((layer) => [layer.low, layer.high])).toEqual([
      [1, 63],
      [1, 63],
      [64, 127],
    ]);
    expect(roundRobinCount(snare)).toBe(2);
    expect(kit.pads[0].gainDb).toBe(-2);
    expect(kit.pads[0].oneShot).toBe(true);
    expect(snare.oneShot).toBe(false);
  });

  it("round-trips pad edits into mutation params", () => {
    const kit = kitFromPreset(kitPreset());
    const pad = { ...kit.pads[0], gainDb: -6, decay: 0.2, reverse: true };
    const updates = new Map(padParamUpdates(pad));
    expect(updates.get("pad.kick.gain")).toBe("-6dB");
    expect(updates.get("pad.kick.decay")).toBe("0.2s");
    expect(updates.get("pad.kick.reverse")).toBe("true");
    expect(updates.get("layer.kick.0.sample")).toBe("samples/kick.wav");
  });

  it("creates a pad when dropping onto an empty slot", () => {
    const kit = kitFromPreset(kitPreset());
    const updates = new Map(withPadSample(kit, "tom_high", "/media/tom.wav"));
    expect(updates.get("pad.tom_high.note")).toBe(String(nextFreeNote(kit)));
    expect(updates.get("pad.tom_high.one_shot")).toBe("true");
    expect(updates.get("layer.tom_high.0.sample")).toBe("/media/tom.wav");
    expect(nextFreeNote(kit)).toBe(DEFAULT_PAD_NOTES.find((note) => ![36, 38].includes(note)));
  });

  it("replaces the first layer when dropping onto an existing pad", () => {
    const kit = kitFromPreset(kitPreset());
    const updates = new Map(withPadSample(kit, "kick", "/media/kick2.wav"));
    expect(updates.get("layer.kick.0.sample")).toBe("/media/kick2.wav");
    expect(updates.has("pad.kick.gain")).toBe(false);
  });

  it("sanitizes pad names into DSL identifiers", () => {
    expect(sanitizePadName("Low Tom 3")).toBe("Low_Tom_3");
    expect(sanitizePadName("3rd hit")).toBe("pad_3rd_hit");
    expect(sanitizePadName("")).toBe("pad");
  });
});

function hit(drum: string, beat = 1, subdivision = 0, modifiers: [string, ParamValue][] = []): HitEvent {
  return { drum, position: { beat, subdivision }, dynamics: null, articulation: null, modifiers };
}

describe("drum lane rows", () => {
  it("lists the kit's pads in its own order, then Other for an unmapped note", () => {
    const kit = kitFromPreset(kitPreset());

    expect(kitLaneRows(kit, []).map((row) => row.label)).toEqual(["kick", "snare"]);

    const rows = kitLaneRows(kit, [42, 50, 38]);
    expect(rows.map((row) => row.label)).toEqual(["kick", "snare", "Other 42", "Other 50"]);
    expect(rows.map((row) => row.note)).toEqual([36, 38, 42, 50]);
    expect(rows.map((row) => row.pad)).toEqual(["kick", "snare", null, null]);
  });

  it("resolves a hit's row from its written note, its pad, then General MIDI", () => {
    const kit = kitFromPreset(kitPreset());

    // A written note wins, so a hit keeps the note it was placed on.
    expect(hitNote(hit("pad_50", 1, 0, [["midi_note", { Number: 50 }]]), kit)).toBe(50);
    // A pad name wins over the name's General MIDI meaning.
    expect(hitNote(hit("kick"), kit)).toBe(36);
    // A name the kit does not declare falls back to General MIDI, which is how
    // a hat written as `hihat` still sounds the kit's own hat.
    expect(hitNote(hit("hihat"), kit)).toBe(42);
    expect(hitNote(hit("nonsense"), kit)).toBe(38);
  });

  it("adds a hit's microtiming to its written sixteenth", () => {
    expect(hitStartQuarters(hit("kick", 2, 1))).toBeCloseTo(1.25, 9);
    expect(
      hitStartQuarters(hit("kick", 1, 1, [["timing_offset_beats", { Number: 1 / 12 }]])),
    ).toBeCloseTo(0.25 + 1 / 12, 9);
    expect(
      hitStartQuarters(hit("kick", 2, 0, [["timing_offset_beats", { Number: -0.125 }]])),
    ).toBeCloseTo(0.875, 9);
  });

  it("names a placed hit after the pad that holds the note, else by its note", () => {
    const kit = kitFromPreset(kitPreset());

    expect(drumForNote(36, kit)).toEqual({ drum: "kick", midiNote: null });
    expect(drumForNote(50, kit)).toEqual({ drum: "pad_50", midiNote: 50 });
    expect(drumForNote(36, null)).toEqual({ drum: "pad_36", midiNote: 36 });
  });
});
