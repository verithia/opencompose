import { describe, expect, it } from "vitest";
import { SYNTH_CATALOG, specForConcreteKey } from "../data/synthCatalog";
import { parameterFromPosition, parameterKeyValue, parameterPosition } from "../components/synth/numericRange";

describe("canonical numeric synth ranges", () => {
  it("reaches exact endpoints and finite travel for every slider including zero-Hz logarithmic LFOs", () => {
    const sliders = SYNTH_CATALOG.params.filter((spec) => spec.control === "slider");
    for (const spec of sliders) {
      expect(parameterFromPosition(spec, 0), spec.ids[0]).toBe(spec.min);
      expect(parameterFromPosition(spec, 1), spec.ids[0]).toBe(spec.max);
      expect(parameterPosition(spec, spec.min), spec.ids[0]).toBe(0);
      expect(parameterPosition(spec, spec.max), spec.ids[0]).toBeCloseTo(1, 12);
      for (const position of [0.01, 0.25, 0.5, 0.75, 0.99]) {
        const value = parameterFromPosition(spec, position);
        expect(Number.isFinite(value), spec.ids[0]).toBe(true);
        expect(value, spec.ids[0]).toBeGreaterThanOrEqual(spec.min);
        expect(value, spec.ids[0]).toBeLessThanOrEqual(spec.max);
      }
    }
  });

  it("uses physical schema steps and exact limits for indexed control families", () => {
    for (const key of ["env3.attack", "lfo10.rate", "macro.8", "mod.64.amount", "mod.64.slew_up", "step.16"]) {
      const spec = specForConcreteKey(key)!;
      expect(spec, key).toBeDefined();
      expect(parameterKeyValue(spec, spec.max, "ArrowRight"), key).toBe(spec.max);
      expect(parameterKeyValue(spec, spec.min, "ArrowLeft"), key).toBe(spec.min);
      expect(parameterKeyValue(spec, spec.min, "End"), key).toBe(spec.max);
      expect(parameterKeyValue(spec, spec.max, "Home"), key).toBe(spec.min);
      expect(parameterKeyValue(spec, spec.default, "ArrowRight"), key)
        .toBeCloseTo(Math.min(spec.max, Math.round((spec.default + spec.step) / spec.step) * spec.step));
    }
  });
});
