import { unitDefinition, type UnitId } from "../data/units.gen";

/** Physical values stay unchanged; only display coordinates use the unit scale. */
export function formatParameterValue(value: number, physicalStep: number, unitId: UnitId): string {
  const unit = unitDefinition(unitId);
  const display = value * unit.displayMultiplier;
  const step = physicalStep * unit.displayMultiplier;
  const decimals = step < 0.01 ? 3 : step < 1 ? 2 : step < 10 ? 1 : 0;
  const number = unitId === "fraction" || unitId === "%"
    ? String(Number(display.toFixed(decimals))) : display.toFixed(decimals);
  return `${number}${unit.displayUnit}`;
}
