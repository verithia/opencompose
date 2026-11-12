/**
 * Waveform peak helpers: decode the Rust pyramid payload, pick a level for the
 * current zoom, and aggregate buckets into per-pixel columns.
 */
import type { AudioClip, GainEnvelopePoint } from "../types/project";

export interface PeakLevel {
  bucket_frames: number;
  min: Float32Array;
  max: Float32Array;
  rms: Float32Array;
}

export interface PeakPyramid {
  sample_rate: number;
  frames: number;
  levels: PeakLevel[];
}

const MAGIC = "OCPK";
const HEADER_BYTES = 30;

/** Decode a base64 payload from `audio_clip_peaks`; null when malformed. */
export function decodePeakBase64(payload: string): PeakPyramid | null {
  if (typeof atob !== "function") return null;
  let binary: string;
  try {
    binary = atob(payload);
  } catch {
    return null;
  }
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return decodePeakBytes(bytes);
}

/** Decode the little-endian payload documented in docs/audio-clip.md. */
export function decodePeakBytes(bytes: Uint8Array): PeakPyramid | null {
  if (bytes.length < HEADER_BYTES) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  for (let i = 0; i < 4; i++) {
    if (String.fromCharCode(bytes[i]) !== MAGIC[i]) return null;
  }
  if (view.getUint8(4) !== 1) return null;
  const sampleRate = view.getUint32(14, true);
  const frames = Number(view.getBigUint64(18, true));
  const bodyBytes = view.getUint32(26, true);
  if (!sampleRate || !frames || HEADER_BYTES + bodyBytes > bytes.length) return null;

  const levels: PeakLevel[] = [];
  let cursor = HEADER_BYTES;
  const end = HEADER_BYTES + bodyBytes;
  while (cursor < end) {
    if (end - cursor < 8) return null;
    const bucketFrames = view.getUint32(cursor, true);
    const buckets = view.getUint32(cursor + 4, true);
    cursor += 8;
    if (!bucketFrames || end - cursor < buckets * 12) return null;
    const arrays: Float32Array[] = [];
    for (let array = 0; array < 3; array++) {
      const values = new Float32Array(buckets);
      for (let index = 0; index < buckets; index++) {
        values[index] = view.getFloat32(cursor + index * 4, true);
      }
      cursor += buckets * 4;
      arrays.push(values);
    }
    levels.push({ bucket_frames: bucketFrames, min: arrays[0], max: arrays[1], rms: arrays[2] });
  }
  if (levels.length === 0) return null;
  return { sample_rate: sampleRate, frames, levels };
}

/** Coarsest level that still gives at least one bucket per pixel. */
export function selectPeakLevel(pyramid: PeakPyramid, framesPerPixel: number): PeakLevel {
  let selected = pyramid.levels[0];
  for (const level of pyramid.levels) {
    if (level.bucket_frames <= framesPerPixel && level.bucket_frames >= selected.bucket_frames) {
      selected = level;
    }
  }
  return selected;
}

export interface WaveformColumns {
  min: Float32Array;
  max: Float32Array;
}

type FrameRange = readonly [start: number, end: number];
type ColumnRanges = (start: number, end: number) => readonly FrameRange[];

/** Map one timeline column to the source intervals on either side of a loop seam. */
export function loopedColumnRanges(
  start: number, end: number, offset: number, period: number, frameAt: (seconds: number) => number,
): readonly FrameRange[] {
  const length = end - start;
  if (length >= period) return [[frameAt(offset), frameAt(offset + period)]];
  const first = start % period;
  const last = first + length;
  const startFrame = frameAt(offset + first);
  if (last <= period) return [[startFrame, frameAt(offset + last)]];
  return [[startFrame, frameAt(offset + period)], [frameAt(offset), frameAt(offset + last - period)]];
}

/**
 * Aggregate buckets into one min/max column per pixel.
 * Frames outside the media return silence.
 */
export function waveformColumns(
  level: PeakLevel,
  startFrame: number,
  endFrame: number,
  columns: number,
  rangesAt?: ColumnRanges,
): WaveformColumns {
  const min = new Float32Array(columns);
  const max = new Float32Array(columns);
  if (columns <= 0 || endFrame <= startFrame) return { min, max };
  const span = (endFrame - startFrame) / columns;
  const buckets = level.min.length;
  const mediaFrames = buckets * level.bucket_frames;
  for (let column = 0; column < columns; column++) {
    const ranges = rangesAt
      ? rangesAt(column / columns, (column + 1) / columns)
      : [[startFrame + column * span, startFrame + (column + 1) * span]];
    let low = Infinity;
    let high = -Infinity;
    for (const [firstFrame, lastFrame] of ranges) {
      if (firstFrame >= mediaFrames) continue;
      const first = Math.max(0, Math.floor(firstFrame / level.bucket_frames));
      const last = Math.min(buckets, Math.max(first + 1, Math.ceil(lastFrame / level.bucket_frames)));
      for (let bucket = first; bucket < last; bucket++) {
        low = Math.min(low, level.min[bucket]);
        high = Math.max(high, level.max[bucket]);
      }
    }
    if (Number.isFinite(low)) min[column] = low;
    if (Number.isFinite(high)) max[column] = high;
  }
  return { min, max };
}

/** Clip gain range the timeline accepts, in dB. */
export const CLIP_GAIN_MIN_DB = -60;
export const CLIP_GAIN_MAX_DB = 24;
/** Shortest fade the timeline accepts; a shorter ramp clicks instead of fading. */
export const MIN_FADE_SECONDS = 0.02;
/** Fraction of the half-height the gain line spans at either limit. */
const GAIN_SPAN = 0.8;

export function gainLinear(gainDb: number): number {
  return Math.pow(10, gainDb / 20);
}

/**
 * Vertical position of the clip gain line: unity sits on the block's middle,
 * the ceiling near the top and the floor near the bottom.
 */
export function gainLineY(gainDb: number, height: number): number {
  const half = height / 2;
  const t = gainDb < 0 ? gainDb / -CLIP_GAIN_MIN_DB : gainDb / CLIP_GAIN_MAX_DB;
  const clamped = Math.max(-1, Math.min(1, t));
  return half - clamped * half * GAIN_SPAN;
}

/** The gain a pointer at `y` sets; the inverse of `gainLineY`. */
export function gainAtY(y: number, height: number): number {
  const half = height / 2;
  const t = (half - y) / (half * GAIN_SPAN);
  const gain = t < 0 ? t * -CLIP_GAIN_MIN_DB : t * CLIP_GAIN_MAX_DB;
  return Math.max(CLIP_GAIN_MIN_DB, Math.min(CLIP_GAIN_MAX_DB, gain));
}

/** "-3.0 dB" with one decimal, the step the project stores. */
export function formatClipGain(gainDb: number): string {
  return `${gainDb > 0 ? "+" : ""}${gainDb.toFixed(1)} dB`;
}

/** "0.25 s" without trailing zeros. */
export function formatClipSeconds(seconds: number): string {
  const text = (seconds >= 1 ? seconds.toFixed(2) : seconds.toFixed(3))
    .replace(/0+$/, "")
    .replace(/\.$/, "");
  return `${text} s`;
}

/** "1.5 q" (quarter notes) without trailing zeros. */
export function formatClipBeats(beats: number): string {
  const text = beats.toFixed(3).replace(/0+$/, "").replace(/\.$/, "");
  return `${text} q`;
}

/** Linear fade envelope at `seconds` into a clip of `duration` seconds. */
export function fadeEnvelope(
  seconds: number,
  duration: number,
  fadeIn: number,
  fadeOut: number,
): number {
  let envelope = 1;
  if (fadeIn > 0 && seconds < fadeIn) envelope *= Math.max(0, seconds / fadeIn);
  if (fadeOut > 0 && seconds > duration - fadeOut) {
    envelope *= Math.max(0, (duration - seconds) / fadeOut);
  }
  return Math.min(1, envelope);
}

/** Media frame span a clip covers, clamped to the decoded file. */
export function mediaFrameRange(
  pyramid: PeakPyramid,
  offsetSeconds: number,
  durationSeconds: number,
): [number, number] {
  const start = Math.min(
    pyramid.frames,
    Math.max(0, offsetSeconds * pyramid.sample_rate),
  );
  const end = Math.max(
    start,
    Math.min(pyramid.frames, (offsetSeconds + durationSeconds) * pyramid.sample_rate),
  );
  return [start, end];
}

export interface WaveformPaint {
  pyramid: PeakPyramid | null;
  startFrame: number;
  endFrame: number;
  rangesAt?: ColumnRanges;
  durationSeconds: number;
  offsetSeconds: number;
  gainDb: number;
  fadeInSeconds: number;
  fadeOutSeconds: number;
  /** Equal-power crossfade at the head and tail, in seconds. */
  crossfadeSeconds?: number;
  /** Optional gain envelope, relative to `gainDb`. */
  gainEnvelope?: GainEnvelopePoint[];
  width: number;
  height: number;
  color: string;
  /** Envelope line and fade ramps; the waveform colour when omitted. */
  envelopeColor?: string;
  /** Outline behind the envelope line, so it stays readable over the waveform. */
  envelopeHalo?: string;
  backgroundColor?: string;
}

/** Cheap canvas paint: one vertical min/max line per pixel column. */
export function paintWaveform(
  context: CanvasRenderingContext2D,
  paint: WaveformPaint,
): void {
  const { width, height } = paint;
  context.clearRect(0, 0, width, height);
  if (paint.backgroundColor) {
    context.fillStyle = paint.backgroundColor;
    context.fillRect(0, 0, width, height);
  }
  const middle = height / 2;
  if (!paint.pyramid) {
    context.strokeStyle = paint.color;
    context.globalAlpha = 0.35;
    context.beginPath();
    context.moveTo(0, middle);
    context.lineTo(width, middle);
    context.stroke();
    context.globalAlpha = 1;
    paintEnvelope(context, paint);
    return;
  }
  const framesPerPixel =
    Math.max(0, paint.endFrame - paint.startFrame) / Math.max(1, width);
  const level = selectPeakLevel(paint.pyramid, framesPerPixel);
  const columns = Math.max(1, Math.round(width));
  const { min, max } = waveformColumns(level, paint.startFrame, paint.endFrame, columns, paint.rangesAt);
  const gain = gainLinear(paint.gainDb);
  const half = middle;
  context.strokeStyle = paint.color;
  context.globalAlpha = 0.85;
  context.lineWidth = 1;
  context.beginPath();
  for (let column = 0; column < columns; column++) {
    const seconds = ((column + 0.5) / columns) * paint.durationSeconds;
    const position = (column + 0.5) / columns;
    const pointGain = paint.gainEnvelope?.length
      ? Math.pow(10, envelopeDb(paint.gainEnvelope, position) / 20)
      : 1;
    let crossfadeGain = 1;
    const crossfadeSpan = paint.durationSeconds > 0
      ? (paint.crossfadeSeconds ?? 0) / paint.durationSeconds
      : 0;
    if (crossfadeSpan > 0) {
      if (position < crossfadeSpan) {
        crossfadeGain *= Math.sin((position / crossfadeSpan) * Math.PI / 2);
      }
      if (position > 1 - crossfadeSpan) {
        crossfadeGain *= Math.sin(((1 - position) / crossfadeSpan) * Math.PI / 2);
      }
    }
    const envelope =
      gain *
      fadeEnvelope(seconds, paint.durationSeconds, paint.fadeInSeconds, paint.fadeOutSeconds) *
      pointGain *
      crossfadeGain;
    const top = middle - Math.max(-1, Math.min(1, max[column] * envelope)) * half;
    const bottom = middle - Math.max(-1, Math.min(1, min[column] * envelope)) * half;
    if (bottom - top < 1) {
      context.moveTo(column + 0.5, middle - 0.5);
      context.lineTo(column + 0.5, middle + 0.5);
      continue;
    }
    context.moveTo(column + 0.5, top);
    context.lineTo(column + 0.5, bottom);
  }
  context.stroke();
  context.globalAlpha = 1;
  paintEnvelope(context, paint);
}

/** Linearly interpolate a relative gain envelope at a normalised position. */
function envelopeDb(points: GainEnvelopePoint[], position: number): number {
  if (points.length === 0) return 0;
  if (position <= points[0].position) return points[0].gain_db;
  for (let index = 1; index < points.length; index++) {
    const previous = points[index - 1];
    const next = points[index];
    if (position <= next.position) {
      const span = next.position - previous.position;
      if (span <= 0) return next.gain_db;
      return previous.gain_db + (next.gain_db - previous.gain_db) * ((position - previous.position) / span);
    }
  }
  return points[points.length - 1].gain_db;
}

/**
 * The clip's gain and fades as one path: a ramp up from silence, the gain line
 * across the clip, then a ramp down. Both fades at zero leave a flat line.
 */
function paintEnvelope(
  context: CanvasRenderingContext2D,
  paint: WaveformPaint,
): void {
  const { width, height } = paint;
  const duration = paint.durationSeconds;
  const gainY = gainLineY(paint.gainDb, height);
  const fadeInX =
    duration > 0 ? Math.min(width, (paint.fadeInSeconds / duration) * width) : 0;
  const fadeOutX =
    duration > 0 ? Math.min(width, (paint.fadeOutSeconds / duration) * width) : 0;
  const flatStart = fadeInX > 0 ? fadeInX : 0;
  const flatEnd = Math.max(flatStart, width - fadeOutX);
  const crossfadeX =
    duration > 0
      ? Math.min(width, ((paint.crossfadeSeconds ?? 0) / duration) * width)
      : 0;
  const envelope = paint.gainEnvelope ?? [];

  const trace = () => {
    context.beginPath();
    if (envelope.length > 0 || crossfadeX > 0) {
      // Sample the real curve: the point envelope, linear fades and the
      // equal-power crossfade all feed the same gain line.
      const steps = Math.max(2, Math.round(width));
      for (let step = 0; step <= steps; step++) {
        const x = (step / steps) * width;
        const position = width > 0 ? x / width : 0;
        let factor = 1;
        if (fadeInX > 0) factor *= Math.min(1, x / fadeInX);
        if (fadeOutX > 0) factor *= Math.min(1, (width - x) / fadeOutX);
        if (crossfadeX > 0) {
          if (x < crossfadeX) factor *= Math.sin((x / crossfadeX) * Math.PI / 2);
          if (x > width - crossfadeX) {
            factor *= Math.sin(((width - x) / crossfadeX) * Math.PI / 2);
          }
        }
        const baseY = gainLineY(paint.gainDb + envelopeDb(envelope, position), height);
        const y = height - (height - baseY) * factor;
        if (step === 0) context.moveTo(x, y);
        else context.lineTo(x, y);
      }
      return;
    }
    if (fadeInX > 0) {
      context.moveTo(0, height);
      context.lineTo(fadeInX, gainY);
    } else {
      context.moveTo(0, gainY);
    }
    context.lineTo(flatEnd, gainY);
    if (fadeOutX > 0) {
      context.lineTo(width, height);
    }
  };

  // A hard chassis-coloured casing keeps the line readable over a busy
  // waveform; the ink core sits on top of it.
  if (paint.envelopeHalo) {
    context.strokeStyle = paint.envelopeHalo;
    context.globalAlpha = 0.95;
    context.lineWidth = 3;
    trace();
    context.stroke();
  }
  context.strokeStyle = paint.envelopeColor ?? paint.color;
  context.globalAlpha = 1;
  context.lineWidth = 1.5;
  trace();
  context.stroke();
  context.globalAlpha = 1;
}

/** Pixel width a clip occupies at a zoom, shared by Timeline and the block. */
export function clipPixels(
  timeRange: AudioClip["time_range"],
  quarterAt: (position: AudioClip["time_range"]["start"]) => number,
  pxPerBeat: number,
): { left: number; width: number } {
  const left = quarterAt(timeRange.start) * pxPerBeat;
  const right = quarterAt(timeRange.end) * pxPerBeat;
  return { left, width: Math.max(2, right - left) };
}
