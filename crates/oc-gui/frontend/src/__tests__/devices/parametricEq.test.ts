import { describe, expect, it } from "vitest";
import fixtureData from "../../devices/__fixtures__/eqResponseTable.json";
import {
  SPECTRUM_BANDS,
  parseShape,
  responseDb,
  spectrumBandCenter,
  spectrumDisplayDb,
  type ParametricBand,
} from "../../devices/parametricEq";

interface FixtureBand {
  slot: string;
  shape: string;
  freq: number;
  q: number;
  gain_db: number;
}

interface FixtureCase {
  name: string;
  bands: FixtureBand[];
  analytic_db: number[];
  measured_db: number[];
}

interface Fixture {
  sample_rate: number;
  frequencies: number[];
  cases: FixtureCase[];
}

const fixture = fixtureData as Fixture;

function toBand(entry: FixtureBand): ParametricBand {
  return {
    id: entry.slot,
    cut: entry.slot.endsWith("_cut"),
    shape: parseShape(entry.shape) ?? "bell",
    freq: entry.freq,
    q: entry.q,
    gainDb: entry.gain_db,
    enabled: true,
    solo: false,
    dynamic: false,
    rangeDb: 0,
    target: "stereo",
  };
}

describe("analyser axis mapping", () => {
  it("places display bands on the same log axis as the curve", () => {
    expect(spectrumBandCenter(0)).toBeGreaterThan(20);
    expect(spectrumBandCenter(SPECTRUM_BANDS - 1)).toBeLessThan(20_000);
    for (let index = 1; index < SPECTRUM_BANDS; index += 1) {
      expect(spectrumBandCenter(index)).toBeGreaterThan(spectrumBandCenter(index - 1));
    }
    let previousX = -1;
    for (let index = 0; index < SPECTRUM_BANDS; index += 1) {
      const x = Math.log(spectrumBandCenter(index) / 20) / Math.log(1000);
      expect(x).toBeGreaterThan(previousX);
      previousX = x;
    }
  });

  it("maps dBFS onto the curve's ±24 dB axis with a +24 dB offset", () => {
    expect(spectrumDisplayDb(0)).toBe(24);
    expect(spectrumDisplayDb(-48)).toBe(-24);
    expect(spectrumDisplayDb(-96)).toBe(-24);
    expect(spectrumDisplayDb(Number.NaN)).toBe(-24);
  });
});

describe("parametric EQ response vs the Rust table", () => {
  it("covers 32 points from 20 Hz to 20 kHz", () => {
    expect(fixture.sample_rate).toBe(48000);
    expect(fixture.frequencies).toHaveLength(32);
    expect(fixture.frequencies[0]).toBeCloseTo(20, 6);
    expect(fixture.frequencies[31]).toBeCloseTo(20000, 6);
  });

  it("matches the measured Rust device within 0.1 dB at every point", () => {
    let worst = 0;
    for (const entry of fixture.cases) {
      const bands = entry.bands.map(toBand);
      fixture.frequencies.forEach((freq, index) => {
        const drawn = responseDb(bands, freq, fixture.sample_rate);
        const rust = entry.analytic_db[index];
        const measured = entry.measured_db[index];
        worst = Math.max(worst, Math.abs(drawn - measured));
        expect(
          Math.abs(drawn - rust),
          `${entry.name} at ${freq.toFixed(1)} Hz: drawn ${drawn.toFixed(4)}, rust ${rust.toFixed(4)}`,
        ).toBeLessThan(0.1);
        expect(
          Math.abs(drawn - measured),
          `${entry.name} at ${freq.toFixed(1)} Hz: drawn ${drawn.toFixed(4)}, measured ${measured.toFixed(4)}`,
        ).toBeLessThan(0.1);
      });
    }
    expect(worst).toBeLessThan(0.01);
  });

  it("keeps the Rust device measurement within 0.1 dB of the same table", () => {
    for (const entry of fixture.cases) {
      fixture.frequencies.forEach((freq, index) => {
        expect(
          Math.abs(entry.measured_db[index] - entry.analytic_db[index]),
          `${entry.name} at ${freq.toFixed(1)} Hz`,
        ).toBeLessThan(0.1);
      });
    }
  });

  it("matches the fixture across every filter shape", () => {
    const shapes = new Set<string>();
    for (const entry of fixture.cases) {
      for (const band of entry.bands) shapes.add(band.shape);
    }
    expect([...shapes].sort()).toEqual([
      "band_pass",
      "bell",
      "high_cut",
      "high_shelf",
      "low_cut",
      "low_shelf",
      "notch",
    ]);
  });
});
