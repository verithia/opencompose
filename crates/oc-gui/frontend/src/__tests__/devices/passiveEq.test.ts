import { describe, expect, it } from "vitest";
import { designBiquad, magnitudeDb } from "../../devices/biquad";
import {
  PASSIVE_EQ_DEFAULTS,
  passiveEqCurve,
  passiveEqDb,
  type PassiveEqValues,
} from "../../devices/passiveEq";

// Expected values come from the Rust device measurements in docs/vocal-tone.md.
function response(overrides: Partial<PassiveEqValues>, freq: number): number {
  return passiveEqDb({ ...PASSIVE_EQ_DEFAULTS, ...overrides }, freq);
}

describe("passive EQ combined response", () => {
  it("does not cancel a 5 dB boost against a 5 dB cut on the shared corner", () => {
    const values = { lowFreq: 100, lowBoost: 5, lowCut: 5 };
    expect(response(values, 100)).toBeCloseTo(2.5, 2);
    expect(response(values, 30)).toBeCloseTo(-4.44, 1);
    expect(response(values, 20)).toBeCloseTo(-4.77, 1);
    expect(response(values, 30)).toBeLessThan(-4);
  });

  it("leaves 1-4 kHz alone for both low corners", () => {
    const values = { lowFreq: 100, lowBoost: 5, lowCut: 5 };
    for (const freq of [1000, 2000, 4000]) {
      expect(Math.abs(response(values, freq))).toBeLessThan(0.3);
    }
  });

  it("leaves 1-4 kHz alone for the high cut and air shelves", () => {
    const values = { highCut: 5, highCutFreq: 10000, air: 6, airFreq: 10000 };
    for (const freq of [1000, 2000, 4000]) {
      expect(Math.abs(response(values, freq))).toBeLessThan(0.3);
    }
  });

  it("spans a 6 dB tilt from -3 to +3 dB instead of 0 to +6", () => {
    expect(response({ tilt: 6 }, 20)).toBeCloseTo(-3, 1);
    expect(response({ tilt: 6 }, 1000)).toBeCloseTo(0, 1);
    expect(response({ tilt: 6 }, 20000)).toBeCloseTo(3, 1);
    expect(response({ tilt: -6 }, 20)).toBeCloseTo(3, 1);
    expect(response({ tilt: -6 }, 20000)).toBeCloseTo(-3, 1);
  });

  it("draws the first-order tilt shelf with the centring term", () => {
    const coefficients = designBiquad("tilt_shelf", 1000, 1, 6);
    expect(magnitudeDb(coefficients, 20)).toBeCloseTo(-2.997, 2);
    expect(magnitudeDb(coefficients, 20000)).toBeCloseTo(3.017, 2);
    const flat = designBiquad("tilt_shelf", 1000, 1, 0);
    expect(magnitudeDb(flat, 20000)).toBeCloseTo(0, 9);
  });

  it("draws the high bell, high shelf and air band", () => {
    expect(response({ highFreq: 8000, highQ: 1.4, highBoost: 5 }, 8000)).toBeCloseTo(5, 2);
    expect(Math.abs(response({ highFreq: 8000, highQ: 1.4, highBoost: 5 }, 1000))).toBeLessThan(0.1);
    expect(response({ highCutFreq: 10000, highCut: 5 }, 10000)).toBeCloseTo(-2.5, 2);
    expect(response({ highCutFreq: 10000, highCut: 5 }, 20000)).toBeCloseTo(-4.99, 2);
    expect(response({ airFreq: 10000, air: 6 }, 20000)).toBeCloseTo(5.988, 2);
    expect(response({ airFreq: 10000, air: 6 }, 10000)).toBeCloseTo(3, 2);
    expect(response({ airFreq: 10000, air: 6 }, 4000)).toBeCloseTo(0.095, 2);
  });

  it("is flat at the defaults and the drawn curve is the summed response", () => {
    for (const freq of [20, 100, 1000, 10000, 20000]) {
      expect(passiveEqDb(PASSIVE_EQ_DEFAULTS, freq)).toBeCloseTo(0, 9);
    }
    const values: PassiveEqValues = {
      ...PASSIVE_EQ_DEFAULTS,
      lowFreq: 120,
      lowBoost: 4,
      lowCut: 3,
      highBoost: 2,
      tilt: 2,
      air: 3,
    };
    const curve = passiveEqCurve(values, 180);
    expect(curve).toHaveLength(180);
    expect(curve[0].freq).toBeCloseTo(20, 6);
    expect(curve[curve.length - 1].freq).toBeCloseTo(20000, 6);
    for (const point of curve.filter((_, index) => index % 30 === 0)) {
      expect(point.db).toBeCloseTo(passiveEqDb(values, point.freq), 6);
    }
  });
});
