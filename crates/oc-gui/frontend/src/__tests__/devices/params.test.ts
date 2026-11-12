import { describe, expect, it } from "vitest";
import {
  clampParam,
  formatParamValue,
  normalizedToValue,
  readEffectParam,
  toParamString,
  valueToNormalized,
} from "../../devices/params";
import { eqCurve, eqMagnitudeDb } from "../../devices/eqResponse";
import { FALLBACK_DEVICES } from "../../devices/catalog";

const eqDevice = FALLBACK_DEVICES.find((device) => device.id === "eq")!;
const lowFreq = eqDevice.params.find((param) => param.id === "low_freq")!;
const lowGain = eqDevice.params.find((param) => param.id === "low_gain")!;
const midGain = eqDevice.params.find((param) => param.id === "mid_gain")!;
const msDef = { ...lowGain, unit: "ms" as const, min: 1, max: 2000, default: 250 };
const percentDef = { ...lowGain, unit: "%" as const, min: 0, max: 100, default: 50 };

describe("device parameter schema helpers", () => {
  it("displays fractional mix as percent but preserves physical writes and static percentage conversion", () => {
    for (const id of ["compression", "multiband"]) {
      const mix = FALLBACK_DEVICES.find((device) => device.id === id)!.params.find((param) => param.id === "mix")!;
      expect(mix.unit).toBe("fraction");
      expect(formatParamValue(mix, 0.4)).toBe("40%");
      expect(toParamString(mix, 0.4)).toBe("0.4");
      expect(readEffectParam({ effect_type: "Compression", params: [["mix", { Percentage: 40 }]] } as never, id, mix)).toBe(0.4);
      expect(readEffectParam({ effect_type: "Compression", params: [["mix", { Percentage: 0.4 }]] } as never, id, mix)).toBe(0.004);
    }
    expect(toParamString(percentDef, 40)).toBe("40%");
  });

  it("clamps values to the schema range and snaps discrete steps", () => {
    expect(clampParam(lowFreq, -100)).toBe(lowFreq.min);
    expect(clampParam(lowFreq, 999999)).toBe(lowFreq.max);
    expect(clampParam(lowGain, -40)).toBe(lowGain.min);
    expect(clampParam(lowGain, 40)).toBe(lowGain.max);
    const discrete = { ...lowGain, steps: 5 };
    const middle = (lowGain.min + lowGain.max) / 2;
    const step = (lowGain.max - lowGain.min) / (discrete.steps - 1);
    expect(clampParam(discrete, middle + step * 0.49)).toBe(middle);
    expect(clampParam(discrete, middle + step * 0.51)).toBe(middle + step);
  });

  it("formats Hz, dB, ms, %, and ratio readouts", () => {
    expect(formatParamValue(lowFreq, 250)).toBe("250 Hz");
    expect(formatParamValue(lowFreq, 20000)).toBe("20 kHz");
    expect(formatParamValue(lowGain, -3)).toBe("-3.0 dB");
    expect(formatParamValue(lowGain, 6)).toBe("+6.0 dB");
    expect(formatParamValue(msDef, 375)).toBe("375 ms");
    expect(formatParamValue(msDef, 2000)).toBe("2 s");
    expect(formatParamValue(percentDef, 35)).toBe("35%");
    expect(formatParamValue({ ...lowGain, unit: "ratio" }, 4)).toBe("4:1");
    expect(formatParamValue(midGain, 0)).toBe("0.0 dB");
  });

  it("serializes values as typed strings for the backend parser", () => {
    expect(toParamString(lowFreq, 250)).toBe("250Hz");
    expect(toParamString(lowFreq, 999999)).toBe("20000Hz");
    expect(toParamString(lowGain, -3.04)).toBe("-3.04dB");
    expect(toParamString(msDef, 375.5)).toBe("375.5ms");
    expect(toParamString(percentDef, 35)).toBe("35%");
    expect(toParamString({ ...lowGain, unit: "ratio" }, 4)).toBe("4");
  });

  it("maps log curves both ways", () => {
    expect(normalizedToValue(lowFreq, 0)).toBeCloseTo(20);
    expect(normalizedToValue(lowFreq, 1)).toBeCloseTo(20000);
    expect(normalizedToValue(lowFreq, 0.5)).toBeCloseTo(632.46, 1);
    expect(valueToNormalized(lowFreq, 200)).toBeCloseTo(Math.log(10) / Math.log(1000), 5);
    expect(valueToNormalized(lowGain, 9)).toBeCloseTo((9 - lowGain.min) / (lowGain.max - lowGain.min), 5);
    const upperQuarter = lowGain.min + 0.75 * (lowGain.max - lowGain.min);
    expect(valueToNormalized(lowGain, upperQuarter)).toBeCloseTo(0.75, 5);
    expect(normalizedToValue(lowGain, 0.75)).toBeCloseTo(upperQuarter, 5);
  });

  it("computes an EQ curve from the same band params", () => {
    expect(eqMagnitudeDb([], 1000)).toBe(0);
    const boosted = [{ id: "mid" as const, freq: 1000, gain: 12, q: 1 }];
    expect(eqCurve(boosted, 3).length).toBe(3);
    expect(eqMagnitudeDb(boosted, 1000)).toBeCloseTo(12, 1);
    expect(Math.abs(eqMagnitudeDb(boosted, 20))).toBeLessThan(1);
  });

  it("serializes and reads boolean and enum parameters", () => {
    const toggle = {
      id: "band1_enabled", name: "Band 1 Enabled", unit: "none" as const,
      min: 0, max: 1, default: 0, curve: "linear" as const, steps: 2, kind: "bool" as const,
    };
    expect(toParamString(toggle, 1)).toBe("true");
    expect(toParamString(toggle, 0)).toBe("false");
    expect(formatParamValue(toggle, 1)).toBe("On");
    expect(formatParamValue(toggle, 0)).toBe("Off");

    const type = {
      id: "band1_type", name: "Band 1 Type", unit: "none" as const,
      min: 0, max: 5, default: 0, curve: "linear" as const, steps: 6,
      kind: "enum" as const,
      options: ["Bell", "Low Shelf", "High Shelf", "Low Cut", "High Cut", "Notch"],
    };
    expect(toParamString(type, 3)).toBe("low_cut");
    expect(formatParamValue(type, 3)).toBe("Low Cut");

    const effect = {
      effect_type: "DynamicEq" as const,
      params: [["band1_type", { String: "low_shelf" }]] as const,
    };
    expect(readEffectParam(effect as never, "dynamic-eq", type)).toBe(1);
    expect(readEffectParam(effect as never, "dynamic-eq", toggle)).toBe(0);
  });
});
