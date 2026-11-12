/**
 * Meter formatting and the ten-second level history.
 *
 * Nothing here invents a reading: an absent value stays absent all the way to
 * the drawn trace, which breaks at a gap instead of drawing a floor level.
 */
import { describe, expect, it } from "vitest";
import {
  METER_HISTORY_CAPACITY,
  METER_HISTORY_SECONDS,
  MeterHistory,
  levelFraction,
  pointsAttribute,
  traceSegments,
} from "../utils/meterHistory";
import {
  BAR_CEILING_DB,
  BAR_FLOOR_DB,
  formatCorrelation,
  formatLevelDb,
  formatLufs,
  levelPercent,
} from "../utils/meterFormat";

describe("meter formatting", () => {
  it("prints a dash for an absent level and never 0 dBFS", () => {
    expect(formatLevelDb(null)).toBe("—");
    expect(formatLevelDb(undefined)).toBe("—");
    expect(formatLevelDb(Number.NaN)).toBe("—");
    expect(formatLevelDb(-6.02)).toBe("-6.0");
    expect(formatLevelDb(0)).toBe("0.0");
    expect(formatLevelDb(3.01)).toBe("+3.0");
  });

  it("formats correlation with two decimals and a sign", () => {
    expect(formatCorrelation(1)).toBe("+1.00");
    expect(formatCorrelation(-1)).toBe("-1.00");
    expect(formatCorrelation(-0.425)).toBe("-0.42");
    expect(formatCorrelation(0)).toBe("+0.00");
    expect(formatCorrelation(null)).toBe("—");
    expect(formatCorrelation(Number.POSITIVE_INFINITY)).toBe("—");
    // Out-of-range values clamp rather than stretching the scale.
    expect(formatCorrelation(3)).toBe("+1.00");
  });

  it("formats LUFS with one decimal", () => {
    expect(formatLufs(-14.24)).toBe("-14.2");
    expect(formatLufs(null)).toBe("—");
  });

  it("maps a dBFS reading onto the drawn bar", () => {
    expect(levelPercent(BAR_FLOOR_DB)).toBe(0);
    expect(levelPercent(BAR_CEILING_DB)).toBe(100);
    expect(levelPercent(-30)).toBe(50);
    expect(levelPercent(-120)).toBe(0);
    expect(levelPercent(6)).toBe(100);
    expect(levelPercent(null)).toBe(0);
  });
});

describe("level history", () => {
  it("keeps exactly the last ten seconds of polls", () => {
    const history = new MeterHistory();
    expect(history.length).toBe(0);
    for (let index = 0; index < METER_HISTORY_CAPACITY + 30; index += 1) {
      history.push(-index / 10);
    }
    const values = history.values();
    expect(values).toHaveLength(METER_HISTORY_CAPACITY);
    expect(METER_HISTORY_CAPACITY).toBe(METER_HISTORY_SECONDS * 10);
    // The oldest thirty samples fell off; the newest is last.
    expect(values[0]).toBeCloseTo(-3.0);
    expect(values[values.length - 1]).toBeCloseTo(-12.9);
  });

  it("keeps a gap as a gap", () => {
    const history = new MeterHistory();
    history.push(-6);
    history.push(null);
    history.push(-12);
    expect(history.values()).toEqual([-6, null, -12]);
    history.reset();
    expect(history.values()).toEqual([]);
  });

  it("rejects a capacity that cannot hold a sample", () => {
    expect(() => new MeterHistory(0)).toThrow(RangeError);
  });

  it("splits a trace at a gap and spans the full window", () => {
    const values: (number | null)[] = new Array(METER_HISTORY_CAPACITY).fill(-20);
    values[40] = null;
    values[80] = null;
    const segments = traceSegments(values, 100, 24);
    expect(segments).toHaveLength(3);
    // Oldest sample sits at the left edge, newest at the right edge.
    expect(segments[0][0].x).toBe(0);
    const last = segments[segments.length - 1];
    expect(last[last.length - 1].x).toBeCloseTo(100);
    // A louder sample draws higher up the box.
    const quiet = traceSegments([-60, -60], 100, 24)[0];
    const loud = traceSegments([0, 0], 100, 24)[0];
    expect(quiet[0].y).toBe(24);
    expect(loud[0].y).toBe(0);
    expect(levelFraction(-30)).toBe(0.5);
  });

  it("draws a partial history at the right edge so its age is readable", () => {
    const segments = traceSegments([-20, -20, -20], 100, 24);
    expect(segments).toHaveLength(1);
    expect(segments[0][0].x).toBeCloseTo(
      ((METER_HISTORY_CAPACITY - 3) / (METER_HISTORY_CAPACITY - 1)) * 100,
    );
    expect(segments[0][2].x).toBeCloseTo(100);
  });

  it("serializes points for an SVG polyline", () => {
    expect(pointsAttribute([{ x: 0, y: 24 }, { x: 50.25, y: 12 }])).toBe("0.0,24.0 50.3,12.0");
  });
});
