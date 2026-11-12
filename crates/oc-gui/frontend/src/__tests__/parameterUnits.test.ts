import { describe, expect, it } from "vitest";
import { formatParameterValue } from "../utils/parameterUnits";
import { specForConcreteKey } from "../data/synthCatalog";

describe("shared physical parameter units", () => {
  it("scales fractional displays including bipolar and above-unity values", () => {
    for (const [physical, display] of [[0.4, "40%"], [1.5, "150%"], [-0.4, "-40%"]] as const) {
      expect(formatParameterValue(physical, 0.01, "fraction")).toBe(display);
    }
    expect(formatParameterValue(40, 1, "%")).toBe("40%");
  });
  it("keeps synth fractional control steps in physical coordinates", () => {
    for (const key of ["macro.1", "step.1", "mod.1.amount", "osc.a.level"]) {
      const spec = specForConcreteKey(key)!;
      expect(spec.unit, key).toBe("fraction");
      expect(formatParameterValue(0.4, spec.step, spec.unit), key).toBe("40%");
      expect(spec.step).toBeLessThanOrEqual(0.01);
    }
  });
});
