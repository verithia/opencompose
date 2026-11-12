import { describe, expect, it } from "vitest";
import {
  estimateExportBytes,
  formatBytes,
  formatDuration,
  formatMp3Description,
  formatSampleDescription,
  stemFileNames,
  stemSlug,
} from "../utils/exportEstimate";

describe("export estimates", () => {
  it("grows with the sample depth and the sample rate", () => {
    const base = { container: "wav" as const, seconds: 60, sampleRate: 44100 };
    const i16 = estimateExportBytes({ ...base, depth: "i16" })!;
    const i24 = estimateExportBytes({ ...base, depth: "i24" })!;
    const f32 = estimateExportBytes({ ...base, depth: "f32" })!;
    expect(i16).toBeLessThan(i24);
    expect(i24).toBeLessThan(f32);

    const fast = estimateExportBytes({ ...base, depth: "i16", sampleRate: 96000 })!;
    expect(fast).toBeGreaterThan(i16 * 2);
  });

  it("estimates each container differently, so the format changes the number", () => {
    const base = { seconds: 120, sampleRate: 48000, depth: "i24" as const };
    const wav = estimateExportBytes({ ...base, container: "wav" })!;
    const flac = estimateExportBytes({ ...base, container: "flac" })!;
    const mp3 = estimateExportBytes({ ...base, container: "mp3", bitrateKbps: 192 })!;
    expect(flac).toBeLessThan(wav);
    expect(flac).toBeGreaterThan(wav / 2);
    expect(mp3).toBeLessThan(flac);
    // 192 kbps for two minutes is about 2.9 MB.
    expect(mp3).toBeGreaterThan(2_800_000);
    expect(mp3).toBeLessThan(3_000_000);
  });

  it("estimates MP3 from the CBR bitrate, not from the depth or the rate", () => {
    const base = { container: "mp3" as const, seconds: 120, sampleRate: 48000, depth: "i16" as const };
    const at192 = estimateExportBytes({ ...base, bitrateKbps: 192 })!;
    const at320 = estimateExportBytes({ ...base, bitrateKbps: 320 })!;
    const at96 = estimateExportBytes({ ...base, bitrateKbps: 96 })!;
    // Linear in the bitrate: the size is bitrate × duration, plus the tag.
    expect(at320 / at192).toBeCloseTo(320 / 192, 2);
    expect(at96 / at192).toBeCloseTo(96 / 192, 2);
    // A different depth or rate cannot move it: Layer III has neither.
    expect(estimateExportBytes({ ...base, bitrateKbps: 192, depth: "i24" })).toBe(at192);
    expect(estimateExportBytes({ ...base, bitrateKbps: 192, sampleRate: 44100 })).toBe(at192);
  });

  it("describes the MP3 rate control and the rate in the summary", () => {
    expect(formatMp3Description(192, 44100, null)).toBe("192 kbps CBR / 44.1 kHz");
    expect(formatMp3Description(320, 48000, null)).toBe("320 kbps CBR / 48 kHz");
    expect(formatMp3Description(192, 44100, 2)).toBe("VBR q2 / 44.1 kHz");
  });

  it("makes the FLAC level change the estimated size, not the depth", () => {
    const base = {
      container: "flac" as const,
      seconds: 120,
      sampleRate: 48000,
      depth: "i24" as const,
    };
    const fastest = estimateExportBytes({ ...base, flacLevel: 0 })!;
    const defaultLevel = estimateExportBytes({ ...base, flacLevel: 5 })!;
    const smallest = estimateExportBytes({ ...base, flacLevel: 8 })!;
    expect(smallest).toBeLessThan(defaultLevel);
    expect(defaultLevel).toBeLessThan(fastest);

    // Measured on the reference project: 24-bit FLAC is about 70% of a
    // same-depth WAV at level 5, 16-bit about 49%.
    const wav24 = estimateExportBytes({ ...base, container: "wav" })!;
    expect(defaultLevel / wav24).toBeGreaterThan(0.68);
    expect(defaultLevel / wav24).toBeLessThan(0.72);
    const wav16 = estimateExportBytes({ ...base, depth: "i16", container: "wav" })!;
    const flac16 = estimateExportBytes({ ...base, depth: "i16", flacLevel: 5 })!;
    expect(flac16 / wav16).toBeGreaterThan(0.48);
    expect(flac16 / wav16).toBeLessThan(0.5);
  });

  it("has no estimate before the arrangement length is known", () => {
    expect(
      estimateExportBytes({ container: "wav", seconds: 0, sampleRate: 44100, depth: "i16" }),
    ).toBeNull();
    expect(
      estimateExportBytes({ container: "wav", seconds: 10, sampleRate: 0, depth: "i16" }),
    ).toBeNull();
  });

  it("writes duration as minutes and seconds, then hours", () => {
    expect(formatDuration(0)).toBe("0:00");
    expect(formatDuration(222)).toBe("3:42");
    expect(formatDuration(3723)).toBe("1:02:03");
  });

  it("names the sample format the way the summary line reads", () => {
    expect(formatSampleDescription("i16", 44100)).toBe("16-bit / 44.1 kHz");
    expect(formatSampleDescription("i24", 48000)).toBe("24-bit / 48 kHz");
    expect(formatSampleDescription("f32", 96000)).toBe("32-bit float / 96 kHz");
  });

  it("prints sizes in the unit a user would say", () => {
    expect(formatBytes(800)).toBe("800 B");
    expect(formatBytes(42_000)).toBe("42 KB");
    expect(formatBytes(3_100_000)).toBe("3.1 MB");
    expect(formatBytes(42_000_000)).toBe("42 MB");
  });

  it("previews the stem files the renderer writes, mix first", () => {
    expect(
      stemFileNames({
        song: "My Song",
        tracks: ["Lead Guitar", "Drums"],
        buses: ["Reverb Bus"],
      }),
    ).toEqual([
      "my-song-mix.wav",
      "my-song-lead-guitar.wav",
      "my-song-drums.wav",
      "my-song-bus-reverb-bus.wav",
    ]);
  });

  it("keeps non-ASCII names instead of falling back to untitled", () => {
    expect(stemSlug("夏日回声", "song")).toBe("夏日回声");
    expect(
      stemFileNames({ song: "夏日回声", tracks: ["主音吉他"], buses: [] }),
    ).toEqual(["夏日回声-mix.wav", "夏日回声-主音吉他.wav"]);
    expect(stemSlug("***", "song")).toBe("song");
  });
});
