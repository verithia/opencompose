/**
 * The shared editing grid: one quarter-note step for the timeline and the
 * piano roll, exact for triplets and 1/32.
 *
 * A block position is a different thing from a grid step. A clip range is
 * written as `bar:beat:subdivision`, whose subdivision divides a beat into
 * four, so a block can only start on a 16th. That floor belongs to block
 * positions alone and the timeline states it; it is not applied to the grid.
 */
import { describe, expect, it } from "vitest";
import {
  BLOCK_POSITION_QUARTERS,
  blockPositionNote,
  blockStepQuarters,
  gridStepQuarters,
  snapBlockQuarter,
  snapQuarterToGrid,
} from "../stores/grid";
import { i18n } from "../stores/i18n";

describe("shared grid step", () => {
  it("keeps 1/32 and 1/8-triplet exact", () => {
    expect(gridStepQuarters(8)).toBeCloseTo(0.125, 12);
    expect(gridStepQuarters(3)).toBeCloseTo(1 / 3, 12);
    expect(gridStepQuarters(6)).toBeCloseTo(1 / 6, 12);
    expect(gridStepQuarters(1)).toBeCloseTo(1, 12);
    expect(gridStepQuarters(4)).toBeCloseTo(0.25, 12);
  });

  it("snaps a 1/32 drag onto the fine grid", () => {
    // The parent floored the step at a quarter beat, so 0.34 landed on 0.25.
    expect(snapQuarterToGrid(0.34, 8)).toBeCloseTo(0.375, 12);
    expect(snapQuarterToGrid(0.34, 8) / 0.125).toBeCloseTo(3, 12);
    expect(snapQuarterToGrid(0.24, 8)).toBeCloseTo(0.25, 12);
    expect(snapQuarterToGrid(0.11, 8)).toBeCloseTo(0.125, 12);
  });

  it("snaps a 1/8-triplet drag onto the triplet line", () => {
    // The parent rounded 1/3 to a 16th, so 0.4 landed on 0.5.
    expect(snapQuarterToGrid(0.4, 3)).toBeCloseTo(1 / 3, 12);
    expect(snapQuarterToGrid(0.7, 3)).toBeCloseTo(2 / 3, 12);
  });

  it("still lands a 1/4 drag on quarters", () => {
    expect(snapQuarterToGrid(0.4, 1)).toBe(0);
    expect(snapQuarterToGrid(0.6, 1)).toBe(1);
    expect(snapQuarterToGrid(2.4, 1)).toBe(2);
    expect(snapQuarterToGrid(-3, 1)).toBe(0);
  });

  it("is the value the piano roll snaps on", () => {
    // The roll calls this function directly, so a note and a drag share one grid.
    for (const division of [1, 2, 3, 4, 6, 8]) {
      const step = 1 / division;
      for (let beat = 0; beat <= 8; beat += 0.05) {
        const snapped = snapQuarterToGrid(beat, division);
        const lines = snapped * division;
        expect(Math.abs(lines - Math.round(lines))).toBeLessThan(1e-9);
        expect(Math.abs(snapped - beat)).toBeLessThanOrEqual(step / 2 + 1e-9);
      }
    }
    expect(snapQuarterToGrid(0.5, 3)).toBeCloseTo(2 / 3, 12);
    expect(snapQuarterToGrid(0.3, 8)).toBeCloseTo(0.25, 12);
  });
});

describe("block position floor", () => {
  it("moves a block on the grid when the grid divides a 16th", () => {
    expect(blockStepQuarters(1)).toBeCloseTo(1, 12);
    expect(blockStepQuarters(2)).toBeCloseTo(0.5, 12);
    expect(blockStepQuarters(4)).toBeCloseTo(BLOCK_POSITION_QUARTERS, 12);
  });

  it("moves a block in 16ths when the grid is finer than a block position", () => {
    expect(blockStepQuarters(8)).toBeCloseTo(BLOCK_POSITION_QUARTERS, 12);
    expect(blockStepQuarters(6)).toBeCloseTo(BLOCK_POSITION_QUARTERS, 12);
    // A triplet line is coarser than a 16th but not a multiple of one.
    expect(blockStepQuarters(3)).toBeCloseTo(BLOCK_POSITION_QUARTERS, 12);
    expect(snapBlockQuarter(0.31, 8)).toBeCloseTo(0.25, 12);
    expect(snapBlockQuarter(0.44, 8)).toBeCloseTo(0.5, 12);
  });

  it("states the reason only when the grid is finer than a block position", () => {
    i18n.setLocale("en");
    expect(blockPositionNote(4)).toBeNull();
    expect(blockPositionNote(2)).toBeNull();
    const note = blockPositionNote(8);
    expect(note).toContain("1/16");
    expect(note).toContain("1/32");
    expect(blockPositionNote(3)).toContain("1/8 triplet");
  });
});
