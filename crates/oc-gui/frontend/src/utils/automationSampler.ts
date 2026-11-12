/**
 * Time-driven sampling for automation recording.
 *
 * A recorded gesture is a sequence of (position, value) samples: one per
 * transport poll while the drag is held, plus one per pointer move. This module
 * turns that sequence into the points a lane can hold, so the rule lives in one
 * place and the store, the panels and the tests agree on it.
 *
 * A point position is written as `bar:beat:subdivision`, so a storable position
 * is a multiple of a 16th note no matter how fine the editing grid is. The
 * sampler snaps to the block step (`snapBlockQuarter`) and keeps the last value
 * of each cell, which means two samples in one cell become one point.
 */
import type { AutomationPosition } from "../stores/synthAutomation";
import { GRID_OPTIONS, blockStepQuarters, gridOptionLabel } from "../stores/grid";

/** One raw sample: a quarter-note position and the control's value there. */
export interface AutomationSample {
  position: number;
  value: number;
}

/** One written point: a snapped quarter-note position and its value. */
export interface SampledPoint {
  quarter: number;
  value: number;
}

/** Grid step a written point actually sits on, in quarter notes. */
export function writeStepQuarters(division: number): number {
  return blockStepQuarters(division);
}

/**
 * The cell one sampled quarter falls in, and the storable quarter that names
 * it. Two positions in one cell produce one point.
 */
export function writeCell(
  division: number,
  quarter: number,
): { cell: number; quarter: number } {
  const step = writeStepQuarters(division);
  const index = Math.max(0, Math.round((Number.isFinite(quarter) ? quarter : 0) / step));
  return { cell: index, quarter: index * step };
}

/** The grid label the report line names, e.g. `1/16` or `1/8 triplet`. */
export function writeGridLabel(division: number): string {
  const value = Math.round(1 / writeStepQuarters(division));
  const option = GRID_OPTIONS.find((candidate) => candidate.value === value);
  return option ? gridOptionLabel(option) : `1/${value}`;
}

/**
 * Collapse a sample sequence into grid points: one per cell, holding the last
 * value that landed there, in ascending order. Non-finite samples are dropped
 * rather than written as a broken point.
 */
export function sampleAutomationPoints(
  division: number,
  samples: readonly AutomationSample[],
): SampledPoint[] {
  const cells = new Map<number, number>();
  for (const sample of samples) {
    if (!Number.isFinite(sample.position) || !Number.isFinite(sample.value)) continue;
    const { cell } = writeCell(division, sample.position);
    cells.set(cell, sample.value);
  }
  return [...cells.entries()]
    .sort(([left], [right]) => left - right)
    .map(([cell, value]) => ({ quarter: cell * writeStepQuarters(division), value }));
}

/** Storable `bar:beat:subdivision` for a quarter-note position. */
export function automationPositionAt(quarter: number, numerator: number): AutomationPosition {
  const beats = Math.max(1, numerator);
  const absolute = Math.max(0, Number.isFinite(quarter) ? quarter : 0);
  const bar = Math.floor(absolute / beats) + 1;
  const beatOffset = absolute - (bar - 1) * beats;
  const beatWhole = Math.floor(beatOffset);
  return {
    bar,
    beat: beatWhole + 1,
    subdivision: Math.min(3, Math.max(0, Math.round((beatOffset - beatWhole) * 4))),
  };
}

/** `3:2:3` for a storable position. */
export function formatAutomationPosition(position: AutomationPosition): string {
  return `${position.bar}:${position.beat}:${position.subdivision}`;
}
