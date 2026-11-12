/**
 * Shared editing grid. The Piano Roll and the Timeline snap to the same
 * division, so the toolbar selector means the same thing in both views.
 */
import { createSignal } from "solid-js";
import { i18n, type I18nKey } from "./i18n";

export const GRID_OPTIONS: ReadonlyArray<{ value: number; label: string }> = [
  { value: 1, label: "1/4" },
  { value: 2, label: "1/8" },
  { value: 3, label: "1/8 triplet" },
  { value: 4, label: "1/16" },
  { value: 6, label: "1/16 triplet" },
  { value: 8, label: "1/32" },
];

/** Fractional divisions stay literal; only the word "triplet" is translated. */
const GRID_LABEL_KEYS: Record<number, I18nKey> = {
  3: "grid.tripletEighth",
  6: "grid.tripletSixteenth",
};

export function gridOptionLabel(option: { value: number; label: string }): string {
  const key = GRID_LABEL_KEYS[option.value];
  return key ? i18n.t(key) : option.label;
}

export const [gridDivision, setGridDivision] = createSignal(4);

/**
 * Quarter-note step of the shared editing grid. Every division stays exact:
 * triplets and 1/32 are not rounded to a coarser line here.
 */
export function gridStepQuarters(division: number): number {
  return 1 / division;
}

/** Snap a quarter-note position onto the shared grid. */
export function snapQuarterToGrid(quarter: number, division: number): number {
  const step = gridStepQuarters(division);
  return Math.max(0, Math.round(quarter / step) * step);
}

/**
 * Quarter-note step a block position can hold. A clip range is written as
 * `bar:beat:subdivision`, whose subdivision divides a beat into four, so a
 * block can only start on a 16th. This is a floor on block positions, never on
 * the grid: the piano roll keeps the exact step, and the timeline shows the
 * reason instead of coarsening silently.
 */
export const BLOCK_POSITION_QUARTERS = 0.25;

/** Step a timeline block actually moves by under the chosen grid. */
export function blockStepQuarters(division: number): number {
  const step = gridStepQuarters(division);
  const ratio = step / BLOCK_POSITION_QUARTERS;
  return Math.abs(ratio - Math.round(ratio)) < 1e-9 ? step : BLOCK_POSITION_QUARTERS;
}

/** Snap a block position onto the shared grid, then onto the storable step. */
export function snapBlockQuarter(quarter: number, division: number): number {
  const step = blockStepQuarters(division);
  return Math.max(0, Math.round(quarter / step) * step);
}

/** The sentence shown when the chosen grid is finer than a block position. */
export function blockPositionNote(division: number): string | null {
  if (Math.abs(blockStepQuarters(division) - gridStepQuarters(division)) < 1e-9) return null;
  const option = GRID_OPTIONS.find((candidate) => candidate.value === division);
  return i18n.t("timeline.blockGridNote", {
    grid: option ? gridOptionLabel(option) : `${division}`,
  });
}

export function resetGridDivision() {
  setGridDivision(4);
}

export const gridStore = {
  division: gridDivision,
  setDivision: setGridDivision,
  stepQuarters: gridStepQuarters,
  snapQuarterToGrid,
  blockStepQuarters,
  snapBlockQuarter,
};
