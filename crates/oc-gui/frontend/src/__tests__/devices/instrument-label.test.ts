import { describe, expect, it } from "vitest";
import { instrumentLabel } from "../../components/PresetEditor";

describe("instrumentLabel", () => {
  it("names the plugin, not the format kind", () => {
    expect(instrumentLabel({ source: "vst3", plugin: "vst3:halcyon_2", program: null })).toBe("Halcyon 2");
    expect(instrumentLabel({ source: "builtin", plugin: "builtin.open_synth", program: null })).toBe("OpenSynth");
    expect(instrumentLabel({ source: "clap", plugin: "clap:surge-xt", program: null })).toBe("Surge XT");
  });

  it("falls back to the source kind when there is no plugin id", () => {
    expect(instrumentLabel({ source: "soundfont", plugin: null, program: 0 })).toBe("SoundFont #0");
    expect(instrumentLabel({ source: "sample", plugin: null, program: null })).toBe("Sample");
    expect(instrumentLabel({ source: "au", plugin: null, program: null })).toBe("Audio Unit");
  });

  it("prettifies other builtins", () => {
    expect(instrumentLabel({ source: "builtin", plugin: "builtin.drumkit", program: null })).toBe("Drum kit");
    expect(instrumentLabel({ source: "builtin", plugin: "builtin.test_tone", program: null })).toBe("Test Tone");
  });
});
