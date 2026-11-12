/**
 * Peak payload decoding, level selection and per-pixel column aggregation.
 */
import { describe, it, expect } from "vitest";
import {
  decodePeakBytes,
  fadeEnvelope,
  gainLinear,
  loopedColumnRanges,
  mediaFrameRange,
  paintWaveform,
  selectPeakLevel,
  waveformColumns,
  type PeakLevel,
  type PeakPyramid,
} from "../utils/waveform";

function level(bucketFrames: number, min: number[], max: number[], rms?: number[]): PeakLevel {
  return {
    bucket_frames: bucketFrames,
    min: Float32Array.from(min),
    max: Float32Array.from(max),
    rms: Float32Array.from(rms ?? min.map(() => 0)),
  };
}

function encode(pyramid: PeakPyramid): Uint8Array {
  let body = 0;
  for (const level of pyramid.levels) body += 8 + level.min.length * 12;
  const bytes = new Uint8Array(30 + body);
  const view = new DataView(bytes.buffer);
  bytes.set([0x4f, 0x43, 0x50, 0x4b], 0); // OCPK
  view.setUint8(4, 1);
  view.setUint32(14, pyramid.sample_rate, true);
  view.setBigUint64(18, BigInt(pyramid.frames), true);
  view.setUint32(26, body, true);
  let cursor = 30;
  for (const level of pyramid.levels) {
    view.setUint32(cursor, level.bucket_frames, true);
    view.setUint32(cursor + 4, level.min.length, true);
    cursor += 8;
    for (const values of [level.min, level.max, level.rms]) {
      for (let i = 0; i < values.length; i++) {
        view.setFloat32(cursor, values[i], true);
        cursor += 4;
      }
    }
  }
  return bytes;
}

describe("peak payload", () => {
  it("decodes every level of a well-formed payload", () => {
    const pyramid: PeakPyramid = {
      sample_rate: 48_000,
      frames: 1024,
      levels: [
        level(256, [-0.5, -0.25], [0.5, 0.25], [0.3, 0.2]),
        level(512, [-0.5], [0.5], [0.25]),
      ],
    };
    const decoded = decodePeakBytes(encode(pyramid));
    expect(decoded).not.toBeNull();
    expect(decoded!.sample_rate).toBe(48_000);
    expect(decoded!.frames).toBe(1024);
    expect(decoded!.levels).toHaveLength(2);
    expect(Array.from(decoded!.levels[0].min)).toEqual([-0.5, -0.25]);
    expect(Array.from(decoded!.levels[0].max)).toEqual([0.5, 0.25]);
    expect(decoded!.levels[0].rms[0]).toBeCloseTo(0.3, 6);
    expect(decoded!.levels[0].rms[1]).toBeCloseTo(0.2, 6);
    expect(decoded!.levels[1].bucket_frames).toBe(512);
  });

  it("rejects a truncated or foreign payload", () => {
    const bytes = encode({
      sample_rate: 48_000,
      frames: 512,
      levels: [level(256, [-0.5], [0.5])],
    });
    expect(decodePeakBytes(bytes.slice(0, 20))).toBeNull();
    const foreign = bytes.slice();
    foreign[0] = 0;
    expect(decodePeakBytes(foreign)).toBeNull();
    const truncated = bytes.slice(0, bytes.length - 4);
    expect(decodePeakBytes(truncated)).toBeNull();
  });
});

describe("level selection", () => {
  const pyramid: PeakPyramid = {
    sample_rate: 48_000,
    frames: 48_000,
    levels: [level(256, [0], [1]), level(512, [0], [1]), level(1024, [0], [1])],
  };

  it("picks the coarsest level that still covers one bucket per pixel", () => {
    expect(selectPeakLevel(pyramid, 100).bucket_frames).toBe(256);
    expect(selectPeakLevel(pyramid, 600).bucket_frames).toBe(512);
    expect(selectPeakLevel(pyramid, 5000).bucket_frames).toBe(1024);
    // A zoom wider than the base bucket falls back to the finest level.
    expect(selectPeakLevel(pyramid, 10).bucket_frames).toBe(256);
  });
});

describe("per-pixel columns", () => {
  it("aggregates every bucket a column covers", () => {
    const buckets = level(2, [0, -1, 0.5, -0.25], [0.5, 0, 1, 0.75]);
    const { min, max } = waveformColumns(buckets, 0, 8, 4);
    expect(Array.from(min)).toEqual([0, -1, 0.5, -0.25]);
    expect(Array.from(max)).toEqual([0.5, 0, 1, 0.75]);
  });

  it("combines only the tail and head of the offset loop when a column crosses the seam", () => {
    const buckets = level(1, [0, -0.5, -1, -0.25], [0, 0.5, 1, 0.25]);
    const { min, max } = waveformColumns(buckets, 1, 6, 1, () =>
      loopedColumnRanges(2.75, 3.25, 1, 3, (seconds) => seconds),
    );
    expect(min[0]).toBe(-0.5);
    expect(max[0]).toBe(0.5);
  });

  it("returns silence outside the decoded file", () => {
    const buckets = level(2, [0.5], [0.5]);
    const { min, max } = waveformColumns(buckets, 8, 16, 2);
    expect(Array.from(min)).toEqual([0, 0]);
    expect(Array.from(max)).toEqual([0, 0]);
  });

  it("does not read past the last bucket on a partial span", () => {
    const buckets = level(4, [0.25, -0.25], [0.5, -0.1]);
    const { min, max } = waveformColumns(buckets, 4, 8, 1);
    expect(min[0]).toBe(-0.25);
    expect(max[0]).toBeCloseTo(-0.1, 6);
  });
});

describe("clip envelope", () => {
  it("applies linear fades and gain", () => {
    expect(fadeEnvelope(0, 4, 1, 0)).toBe(0);
    expect(fadeEnvelope(0.5, 4, 1, 0)).toBe(0.5);
    expect(fadeEnvelope(2, 4, 1, 0)).toBe(1);
    expect(fadeEnvelope(3, 4, 0, 1)).toBe(1);
    expect(fadeEnvelope(3.5, 4, 0, 1)).toBe(0.5);
    expect(fadeEnvelope(4, 4, 0, 1)).toBe(0);
    expect(gainLinear(0)).toBe(1);
    expect(gainLinear(-6)).toBeCloseTo(0.501187, 5);
  });

  it("clamps the media range to the decoded frames", () => {
    const pyramid: PeakPyramid = { sample_rate: 1000, frames: 1000, levels: [level(10, [0], [0])] };
    expect(mediaFrameRange(pyramid, 0.5, 2)).toEqual([500, 1000]);
    expect(mediaFrameRange(pyramid, 2, 1)).toEqual([1000, 1000]);
    expect(mediaFrameRange(pyramid, -1, 0.5)).toEqual([0, 0]);
  });

  it("paints one vertical line per column through the envelope", () => {
    const moves: number[][] = [];
    const context = {
      clearRect: () => {},
      fillRect: () => {},
      beginPath: () => {},
      moveTo: (x: number, y: number) => moves.push([x, y]),
      lineTo: (x: number, y: number) => moves.push([x, y]),
      stroke: () => {},
      globalAlpha: 1,
      lineWidth: 1,
      strokeStyle: "",
      fillStyle: "",
    } as unknown as CanvasRenderingContext2D;
    const pyramid: PeakPyramid = {
      sample_rate: 1000,
      frames: 1000,
      levels: [level(10, new Array(100).fill(-1), new Array(100).fill(1))],
    };
    paintWaveform(context as CanvasRenderingContext2D, {
      pyramid,
      startFrame: 0,
      endFrame: 1000,
      durationSeconds: 1,
      offsetSeconds: 0,
      gainDb: 0,
      fadeInSeconds: 1,
      fadeOutSeconds: 0,
      width: 2,
      height: 40,
      color: "#fff",
    });
    // Column lines sit at x = column + 0.5; the envelope path also moves to the
    // clip edges, so only the column moves are counted here.
    const columns = moves.filter(([x]) => x === 0.5 || x === 1.5);
    expect(columns.length).toBe(4);
    // Column 0 sits a quarter into a one-second fade-in: envelope 0.25.
    expect(columns[0][1]).toBeCloseTo(20 - 0.25 * 20, 5);
    expect(columns[1][1]).toBeCloseTo(20 + 0.25 * 20, 5);
    // Column 1 sits at three quarters: envelope 0.75.
    expect(columns[2][1]).toBeCloseTo(20 - 0.75 * 20, 5);
    expect(columns[3][1]).toBeCloseTo(20 + 0.75 * 20, 5);
  });
});
