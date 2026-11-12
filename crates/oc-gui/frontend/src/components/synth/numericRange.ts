import type { SynthParamSpec } from "../../data/synthCatalog";

type NumericSpec = Pick<SynthParamSpec, "min" | "max" | "step" | "default" | "curve">;

export function parameterValue(spec: NumericSpec, raw: number): number {
  const value = Number.isFinite(raw) ? raw : spec.default;
  if (value <= spec.min) return spec.min;
  if (value >= spec.max) return spec.max;
  return Math.min(spec.max, Math.max(spec.min, Math.round(value / spec.step) * spec.step));
}

export function parameterPosition(spec: NumericSpec, raw: number): number {
  const value = Math.min(spec.max, Math.max(spec.min, Number.isFinite(raw) ? raw : spec.default));
  if (spec.curve === "log") {
    if (spec.min > 0) return Math.log(value / spec.min) / Math.log(spec.max / spec.min);
    // A zero-Hz LFO is valid. One physical step extends logarithmic travel
    // continuously to zero instead of excluding the catalog's lower endpoint.
    return Math.log1p((value - spec.min) / spec.step) / Math.log1p((spec.max - spec.min) / spec.step);
  }
  const normalized = (value - spec.min) / (spec.max - spec.min);
  return spec.curve === "time" ? Math.cbrt(normalized) : normalized;
}

export function parameterFromPosition(spec: NumericSpec, raw: number): number {
  const position = Math.min(1, Math.max(0, Number.isFinite(raw) ? raw : 0));
  if (position === 0) return spec.min;
  if (position === 1) return spec.max;
  if (spec.curve === "log") {
    const value = spec.min > 0 ? spec.min * (spec.max / spec.min) ** position
      : spec.min + spec.step * Math.expm1(position * Math.log1p((spec.max - spec.min) / spec.step));
    return parameterValue(spec, value);
  }
  return parameterValue(spec, spec.min + (spec.max - spec.min) * (spec.curve === "time" ? position ** 3 : position));
}

export function parameterKeyValue(spec: NumericSpec, current: number, key: string): number | null {
  if (key === "Home") return spec.min;
  if (key === "End") return spec.max;
  const direction = key === "ArrowRight" || key === "ArrowUp" ? 1
    : key === "ArrowLeft" || key === "ArrowDown" ? -1 : 0;
  return direction ? parameterValue(spec, current + direction * spec.step) : null;
}
