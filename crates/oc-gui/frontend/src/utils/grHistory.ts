/**
 * Fixed-size gain-reduction history. One sample per meter poll, oldest first,
 * so a panel left open keeps a constant memory footprint.
 */

/** 60 samples at the 10 Hz meter poll cover roughly six seconds. */
export const GR_HISTORY_CAPACITY = 60;
export const GR_HISTORY_SECONDS = 6;
/** The limiter's auto-release depth reference, used as the strip full scale. */
export const GR_HISTORY_RANGE_DB = 12;

export class GrHistory {
  readonly capacity: number;
  private readonly samples: Float32Array;
  private start = 0;
  private count = 0;

  constructor(capacity: number = GR_HISTORY_CAPACITY) {
    if (!Number.isInteger(capacity) || capacity < 1) {
      throw new RangeError("Gain-reduction history needs a positive integer capacity.");
    }
    this.capacity = capacity;
    this.samples = new Float32Array(capacity);
  }

  get length(): number {
    return this.count;
  }

  /** Append one normalised 0..1 sample, dropping the oldest when full. */
  push(value: number): void {
    const sample = Number.isFinite(value) ? Math.min(1, Math.max(0, value)) : 0;
    this.samples[(this.start + this.count) % this.capacity] = sample;
    if (this.count < this.capacity) {
      this.count += 1;
    } else {
      this.start = (this.start + 1) % this.capacity;
    }
  }

  /** Oldest-to-newest copy for rendering; never longer than capacity. */
  values(): number[] {
    const out = new Array<number>(this.count);
    for (let index = 0; index < this.count; index += 1) {
      out[index] = this.samples[(this.start + index) % this.capacity];
    }
    return out;
  }
}
