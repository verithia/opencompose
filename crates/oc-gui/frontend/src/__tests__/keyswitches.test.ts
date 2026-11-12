/**
 * The keyswitch resolver: which key the engine presses for a note's
 * articulation, given the track's params layered over the preset's.
 */
import { describe, it, expect } from "vitest";
import { keyswitchFor, midiNoteName } from "../utils/keyswitches";
import type { ParamValue } from "../types/project";

const params = (entries: Record<string, ParamValue>): [string, ParamValue][] =>
  Object.entries(entries);

describe("keyswitchFor", () => {
  it("reads the key the preset declares for the articulated token", () => {
    const preset = params({ articulation_staccato_keyswitch: { String: "C0" } });
    expect(keyswitchFor("Staccato", null, preset)).toEqual({ note: 12, label: "C0" });
  });

  it("lets the track's engine params win over the preset's", () => {
    const preset = params({ articulation_staccato_keyswitch: { String: "C0" } });
    const track = params({ articulation_staccato_keyswitch: { String: "C1" } });
    expect(keyswitchFor("Staccato", track, preset)).toEqual({ note: 24, label: "C1" });
  });

  it("returns null when the preset declares no keyswitch for that token", () => {
    const preset = params({ articulation_legato_keyswitch: { String: "C0" } });
    expect(keyswitchFor("Staccato", null, preset)).toBeNull();
  });

  it("returns null with no params and with no articulation", () => {
    expect(keyswitchFor("Staccato", null, null)).toBeNull();
    expect(keyswitchFor(null, params({ articulation_staccato_keyswitch: { String: "C0" } }), null)).toBeNull();
  });

  it("resolves a round-robin list to its first key", () => {
    const preset = params({ articulation_staccato_round_robin_keyswitches: { String: "C0 C#0 D0" } });
    expect(keyswitchFor("Staccato", null, preset)).toEqual({ note: 12, label: "C0" });
  });

  it("prefers the round robin over a single keyswitch, as the engine does", () => {
    const preset = params({ articulation_staccato_keyswitch: { String: "C0" } });
    const track = params({ articulation_staccato_round_robin_keyswitches: { String: "D0, E0" } });
    expect(keyswitchFor("Staccato", track, preset)).toEqual({ note: 14, label: "D0" });
  });

  it("accepts a MIDI number as well as a note name", () => {
    expect(keyswitchFor("Staccato", null, params({ articulation_staccato_keyswitch: { Number: 24 } })))
      .toEqual({ note: 24, label: "C1" });
    expect(keyswitchFor("Staccato", null, params({ articulation_staccato_keyswitch: { String: "24" } })))
      .toEqual({ note: 24, label: "C1" });
  });

  it("normalizes the key spelling, as the engine does", () => {
    expect(
      keyswitchFor("Staccato", null, params({ "Articulation.Staccato.Keyswitch": { String: "c0" } })),
    ).toEqual({ note: 12, label: "C0" });
    expect(
      keyswitchFor("Staccato", null, params({ keyswitch_staccato: { String: "D0" } })),
    ).toEqual({ note: 14, label: "D0" });
  });

  it("treats an unreadable value as no keyswitch", () => {
    expect(
      keyswitchFor("Staccato", null, params({ articulation_staccato_keyswitch: { String: "banana" } })),
    ).toBeNull();
    // One bad entry voids the whole round-robin list, as the engine discards it.
    expect(
      keyswitchFor("Staccato", null, params({
        articulation_staccato_round_robin_keyswitches: { String: "C0 banana" },
      })),
    ).toBeNull();
  });
});

describe("midiNoteName", () => {
  it("spells the roll's own octaves, middle C = C4 = 60", () => {
    expect(midiNoteName(60)).toBe("C4");
    expect(midiNoteName(12)).toBe("C0");
    expect(midiNoteName(13)).toBe("C#0");
    expect(midiNoteName(127)).toBe("G9");
  });
});
