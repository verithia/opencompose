/**
 * Waveform paint: the gain line and the fade ramps are real canvas paths, so a
 * clip with an envelope draws differently from a flat clip.
 */
import { describe, it, expect } from "vitest";
import {
  gainLineY,
  paintWaveform,
  type PeakPyramid,
  type WaveformPaint,
} from "../../utils/waveform";

interface Op {
  op: string;
  x?: number;
  y?: number;
}

function recordingContext(): {
  context: CanvasRenderingContext2D;
  ops: Op[];
} {
  const ops: Op[] = [];
  const context = {
    strokeStyle: "",
    fillStyle: "",
    globalAlpha: 1,
    lineWidth: 1,
    clearRect: () => {},
    fillRect: () => {},
    beginPath: () => ops.push({ op: "beginPath" }),
    stroke: () => ops.push({ op: "stroke" }),
    moveTo: (x: number, y: number) => ops.push({ op: "moveTo", x, y }),
    lineTo: (x: number, y: number) => ops.push({ op: "lineTo", x, y }),
  };
  return { context: context as unknown as CanvasRenderingContext2D, ops };
}

/** The path of the last `beginPath`..`stroke` pair: the envelope. */
function lastPath(ops: Op[]): Op[] {
  const start = ops.map((op) => op.op).lastIndexOf("beginPath");
  const end = ops.findIndex((op, index) => index > start && op.op === "stroke");
  if (start < 0) return [];
  return ops.slice(start + 1, end < 0 ? undefined : end);
}

/** The first `beginPath`..`stroke` pair: the waveform columns. */
function columnPath(ops: Op[]): Op[] {
  const start = ops.findIndex((op) => op.op === "beginPath");
  const end = ops.findIndex((op, index) => index > start && op.op === "stroke");
  return ops.slice(start + 1, end);
}

function constantPyramid(): PeakPyramid {
  const buckets = 64;
  return {
    sample_rate: 48_000,
    frames: buckets * 256,
    levels: [
      {
        bucket_frames: 256,
        min: new Float32Array(buckets).fill(-0.5),
        max: new Float32Array(buckets).fill(0.5),
        rms: new Float32Array(buckets).fill(0.25),
      },
    ],
  };
}

function paint(overrides: Partial<WaveformPaint>): Op[] {
  const { context, ops } = recordingContext();
  paintWaveform(context, {
    pyramid: null,
    startFrame: 0,
    endFrame: 0,
    durationSeconds: 4,
    offsetSeconds: 0,
    gainDb: 0,
    fadeInSeconds: 0,
    fadeOutSeconds: 0,
    width: 100,
    height: 40,
    color: "#fff",
    ...overrides,
  });
  return ops;
}

describe("clip envelope paint", () => {
  it("draws a flat gain line for a clip with no gain or fades", () => {
    const path = lastPath(paint({}));
    expect(path).toEqual([
      { op: "moveTo", x: 0, y: 20 },
      { op: "lineTo", x: 100, y: 20 },
    ]);
  });

  it("draws ramps at both ends and the gain line between them", () => {
    const path = lastPath(
      paint({ gainDb: -6, fadeInSeconds: 0.5, fadeOutSeconds: 1 }),
    );
    const line = gainLineY(-6, 40);
    expect(line).toBeCloseTo(21.6, 6);
    expect(path).toHaveLength(4);
    // Ramp up from silence, flat at the gain level, ramp down to silence.
    expect(path[0]).toEqual({ op: "moveTo", x: 0, y: 40 });
    expect(path[1].op).toBe("lineTo");
    expect(path[1].x).toBeCloseTo(12.5, 6);
    expect(path[1].y).toBeCloseTo(line, 6);
    expect(path[2].x).toBeCloseTo(75, 6);
    expect(path[2].y).toBeCloseTo(line, 6);
    expect(path[3]).toEqual({ op: "lineTo", x: 100, y: 40 });
  });

  it("puts the gain line above the middle for a boost", () => {
    const path = lastPath(paint({ gainDb: 6 }));
    expect(path[0].y).toBeCloseTo(16, 6);
    expect(path[0].y).toBeLessThan(20);
  });

  it("paints a clip with an envelope differently from a flat clip", () => {
    const flat = paint({});
    const enveloped = paint({ gainDb: -6, fadeInSeconds: 0.5, fadeOutSeconds: 0.5 });
    expect(enveloped).not.toEqual(flat);
  });

  it("scales the waveform columns by the clip gain", () => {
    const pyramid = constantPyramid();
    const atUnity = columnPath(paint({ pyramid, endFrame: pyramid.frames }));
    const quiet = columnPath(paint({ pyramid, endFrame: pyramid.frames, gainDb: -6 }));
    const span = (path: Op[]) => Math.abs((path[1].y ?? 0) - (path[0].y ?? 0));
    expect(span(atUnity)).toBeCloseTo(20, 3);
    expect(span(quiet)).toBeCloseTo(10, 1);
  });
});
