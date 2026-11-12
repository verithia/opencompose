/**
 * Fixed-size level history: one sample per meter poll, oldest first.
 *
 * The meter polls at 10 Hz, so 100 slots are the last ten seconds. A `null`
 * sample is a real gap (the transport stopped or nothing was measured) and is
 * drawn as a break, never as a level.
 */

/** Samples kept, one per 100 ms poll. */
export const METER_HISTORY_CAPACITY = 100;
/** Seconds the trace covers at that poll rate. */
export const METER_HISTORY_SECONDS = 10;
/** Bottom of the drawn scale, in dBFS; lower levels draw empty. */
export const TRACE_FLOOR_DB = -60;

export class MeterHistory {
  readonly capacity: number;
  private readonly samples: (number | null)[];
  private start = 0;
  private count = 0;

  constructor(capacity: number = METER_HISTORY_CAPACITY) {
    if (!Number.isInteger(capacity) || capacity < 1) {
      throw new RangeError("Level history needs a positive integer capacity.");
    }
    this.capacity = capacity;
    this.samples = new Array<number | null>(capacity).fill(null);
  }

  get length(): number {
    return this.count;
  }

  /** Append one dBFS sample, or `null` for a gap, dropping the oldest. */
  push(db: number | null): void {
    const sample =
      db !== null && Number.isFinite(db) ? Math.max(TRACE_FLOOR_DB, Math.min(12, db)) : null;
    this.samples[(this.start + this.count) % this.capacity] = sample;
    if (this.count < this.capacity) {
      this.count += 1;
    } else {
      this.start = (this.start + 1) % this.capacity;
    }
  }

  /** Drop every sample; the next poll starts a fresh window. */
  reset(): void {
    this.samples.fill(null);
    this.start = 0;
    this.count = 0;
  }

  /** Oldest-to-newest copy for drawing; never longer than capacity. */
  values(): (number | null)[] {
    const out = new Array<number | null>(this.count);
    for (let index = 0; index < this.count; index += 1) {
      out[index] = this.samples[(this.start + index) % this.capacity];
    }
    return out;
  }
}

export interface TracePoint {
  x: number;
  y: number;
}

/** Height fraction for one dBFS sample: 0 at the floor, 1 at 0 dBFS. */
export function levelFraction(db: number): number {
  return Math.min(1, Math.max(0, (db - TRACE_FLOOR_DB) / -TRACE_FLOOR_DB));
}

/**
 * Split a history into contiguous segments of an SVG trace, oldest at the left
 * and newest at the right of the full window. A `null` sample ends a segment,
 * so a stopped transport leaves a gap instead of a fake level.
 */
export function traceSegments(
  values: (number | null)[],
  width: number,
  height: number,
  capacity: number = METER_HISTORY_CAPACITY,
): TracePoint[][] {
  const segments: TracePoint[][] = [];
  let current: TracePoint[] = [];
  const span = Math.max(1, capacity - 1);
  const offset = Math.max(0, capacity - values.length);
  values.forEach((value, index) => {
    if (value === null) {
      if (current.length > 0) segments.push(current);
      current = [];
      return;
    }
    current.push({
      x: ((offset + index) / span) * width,
      y: height * (1 - levelFraction(value)),
    });
  });
  if (current.length > 0) segments.push(current);
  return segments;
}

/** SVG `points` for one segment, so a caller can draw it with a polyline. */
export function pointsAttribute(points: TracePoint[]): string {
  return points.map((point) => `${point.x.toFixed(1)},${point.y.toFixed(1)}`).join(" ");
}
