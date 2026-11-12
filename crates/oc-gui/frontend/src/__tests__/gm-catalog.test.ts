import { describe, expect, it } from "vitest";
import { GM_PROGRAMS } from "../data/gmPrograms.gen";
import { presetCopy } from "../stores/locales/presetCopy";

const PROGRAM_IDS = Array.from({ length: 128 }, (_, id) => id);

describe("Rust-owned General MIDI program catalog", () => {
  it("retains all zero-based program IDs and complete family banks", () => {
    expect(GM_PROGRAMS.map((program) => program.program)).toEqual(PROGRAM_IDS);
    expect(new Set(GM_PROGRAMS.map((program) => program.name)).size).toBe(128);
    const families = [...new Set(GM_PROGRAMS.map((program) => program.family))];
    expect(families).toHaveLength(16);
    for (const family of families) expect(GM_PROGRAMS.filter((program) => program.family === family)).toHaveLength(8);
    expect(GM_PROGRAMS[25]).toMatchObject({ category: "Guitar", name: "Acoustic Guitar (steel)", pickerName: "Steel Guitar" });
    expect(GM_PROGRAMS[33]).toMatchObject({ category: "Bass", name: "Electric Bass (finger)", pickerName: "Finger Bass" });
    expect(GM_PROGRAMS[127]).toMatchObject({ category: "SFX", name: "Gunshot" });
  });
  it("keeps exact program identity covered by translations in every locale", () => {
    const expected = PROGRAM_IDS.map((id) => `preset.program.${id}`).sort();
    for (const [locale, copy] of Object.entries(presetCopy)) {
      const keys = Object.keys(copy).filter((key) => /^preset\.program\.[0-9]+$/.test(key)).sort();
      expect(keys, locale).toEqual(expected);
      for (const key of keys) expect(copy[key as keyof typeof copy].length, `${locale}:${key}`).toBeGreaterThan(0);
    }
  });
});
