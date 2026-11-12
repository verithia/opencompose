import { describe, expect, it } from "vitest";
import { GR_HISTORY_CAPACITY, GrHistory } from "../utils/grHistory";

describe("gain-reduction history ring", () => {
  it("keeps the newest samples oldest-first once it wraps", () => {
    const history = new GrHistory(4);
    history.push(0.1);
    history.push(0.2);
    history.push(0.3);
    expect(history.values()).toHaveLength(3);
    expect(history.values()[0]).toBeCloseTo(0.1);
    expect(history.values()[1]).toBeCloseTo(0.2);
    expect(history.values()[2]).toBeCloseTo(0.3);

    history.push(0.4);
    expect(history.length).toBe(4);
    expect(history.values()[0]).toBeCloseTo(0.1);
    expect(history.values()[3]).toBeCloseTo(0.4);

    history.push(0.5);
    expect(history.values()[0]).toBeCloseTo(0.2);
    expect(history.values()[3]).toBeCloseTo(0.5);
  });

  it("stays bounded and keeps the newest sample after many pushes", () => {
    const history = new GrHistory(GR_HISTORY_CAPACITY);
    for (let index = 0; index < 10_000; index += 1) {
      history.push(index / 10_000);
    }

    expect(history.capacity).toBe(GR_HISTORY_CAPACITY);
    expect(history.length).toBe(GR_HISTORY_CAPACITY);
    expect(history.values()).toHaveLength(GR_HISTORY_CAPACITY);
    expect(history.values().at(-1)).toBeCloseTo(0.9999);
    expect(history.values().at(0)).toBeCloseTo((10_000 - GR_HISTORY_CAPACITY) / 10_000);

    const copy = history.values();
    copy[0] = -1;
    expect(history.values()[0]).toBeGreaterThanOrEqual(0);
  });

  it("clamps non-finite and out-of-range input", () => {
    const history = new GrHistory(4);
    history.push(Number.NaN);
    history.push(Number.POSITIVE_INFINITY);
    history.push(-1);
    history.push(2);

    expect(history.values()).toEqual([0, 0, 0, 1]);
  });

  it("rejects a capacity that cannot hold one sample", () => {
    expect(() => new GrHistory(0)).toThrow(RangeError);
    expect(() => new GrHistory(1.5)).toThrow(RangeError);
  });
});
