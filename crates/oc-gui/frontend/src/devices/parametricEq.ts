/**
 * Parametric EQ band model and summed response; shapes map 1:1 to `dynamic_eq::BandKind`.
 */
import {
  DEFAULT_SAMPLE_RATE,
  MAX_FREQ,
  MIN_FREQ,
  designBiquad,
  logFrequencies,
  magnitudeDb,
  type BiquadShape,
} from "./biquad";

export type ParametricShape =
  | "bell"
  | "low_shelf"
  | "high_shelf"
  | "low_cut"
  | "high_cut"
  | "notch"
  | "band_pass";

export type ChannelTarget = "stereo" | "mid" | "side" | "left" | "right";
export type ChannelView = "all" | ChannelTarget;

/** Views offered by the canvas; "all" sums every enabled band. */
export const CHANNEL_VIEWS: ChannelView[] = ["all", "left", "right", "mid", "side"];

export interface ParametricBand {
  id: string;
  /** True for the device's dedicated low/high cut slots. */
  cut: boolean;
  shape: ParametricShape;
  freq: number;
  q: number;
  gainDb: number;
  enabled: boolean;
  solo: boolean;
  dynamic: boolean;
  rangeDb: number;
  target: ChannelTarget;
}

export const SHAPE_TO_BIQUAD: Record<ParametricShape, BiquadShape> = {
  bell: "peak",
  low_shelf: "low_shelf",
  high_shelf: "high_shelf",
  low_cut: "high_pass",
  high_cut: "low_pass",
  notch: "notch",
  band_pass: "band_pass",
};

/** Display bands published by the engine analyser (20 Hz to 20 kHz). */
export const SPECTRUM_BANDS = 128;
export const SPECTRUM_MIN_HZ = 20;
export const SPECTRUM_MAX_HZ = 20000;
/** 0 dBFS sits at +24 dB on the curve axis, so 1 dB is 1 dB at every zoom. */
export const SPECTRUM_DISPLAY_OFFSET_DB = 24;
export const SPECTRUM_FLOOR_DB = -96;

export const SHAPES: ParametricShape[] = [
  "bell",
  "low_shelf",
  "high_shelf",
  "low_cut",
  "high_cut",
  "notch",
  "band_pass",
];

/** The shape a fixture or schema label refers to, or null when unknown. */
export function parseShape(label: string): ParametricShape | null {
  const normalized = label.trim().toLowerCase().replace(/[\s-]+/g, "_");
  return (SHAPES as string[]).includes(normalized) ? (normalized as ParametricShape) : null;
}

export function parseChannel(label: string): ChannelTarget | null {
  const normalized = label.trim().toLowerCase();
  return normalized === "stereo" ||
    normalized === "mid" ||
    normalized === "side" ||
    normalized === "left" ||
    normalized === "right"
    ? (normalized as ChannelTarget)
    : null;
}

export function shapeUsesGain(shape: ParametricShape): boolean {
  return shape !== "low_cut" && shape !== "high_cut" && shape !== "band_pass";
}

export function soloActive(bands: ParametricBand[]): boolean {
  return bands.some((band) => band.enabled && band.solo);
}

function appliesToView(target: ChannelTarget, view: ChannelView): boolean {
  return view === "all" || target === "stereo" || target === view;
}

/** Bands that both process audio and belong to the viewed channel. */
export function audibleBands(
  bands: ParametricBand[],
  view: ChannelView = "all",
): ParametricBand[] {
  const soloing = soloActive(bands);
  return bands.filter(
    (band) => band.enabled && (!soloing || band.solo) && appliesToView(band.target, view),
  );
}

/** Response at one frequency, in dB; `dynamicRange` draws the full-range edge. */
export function bandResponseDb(
  band: ParametricBand,
  freq: number,
  sampleRate = DEFAULT_SAMPLE_RATE,
  dynamicRange = false,
): number {
  const gain = band.dynamic && dynamicRange ? band.gainDb + band.rangeDb : band.gainDb;
  const coefficients = designBiquad(SHAPE_TO_BIQUAD[band.shape], band.freq, band.q, gain, sampleRate);
  return magnitudeDb(coefficients, freq, sampleRate);
}

export function responseDb(
  bands: ParametricBand[],
  freq: number,
  sampleRate = DEFAULT_SAMPLE_RATE,
  view: ChannelView = "all",
  dynamicRange = false,
): number {
  return audibleBands(bands, view).reduce(
    (total, band) => total + bandResponseDb(band, freq, sampleRate, dynamicRange),
    0,
  );
}

export interface CurvePoint {
  freq: number;
  db: number;
}

/** Summed response sampled across the log axis. */
export function responseCurve(
  bands: ParametricBand[],
  points = 240,
  sampleRate = DEFAULT_SAMPLE_RATE,
  view: ChannelView = "all",
  dynamicRange = false,
): CurvePoint[] {
  const sections = audibleBands(bands, view).map((band) => {
    const gain = band.dynamic && dynamicRange ? band.gainDb + band.rangeDb : band.gainDb;
    return designBiquad(SHAPE_TO_BIQUAD[band.shape], band.freq, band.q, gain, sampleRate);
  });
  return logFrequencies(points, MIN_FREQ, MAX_FREQ).map((freq) => ({
    freq,
    db: sections.reduce((total, section) => total + magnitudeDb(section, freq, sampleRate), 0),
  }));
}

/** Center frequency of analyser display band `index`, matching the engine fold. */
export function spectrumBandCenter(index: number, bands = SPECTRUM_BANDS): number {
  const ratio = SPECTRUM_MAX_HZ / SPECTRUM_MIN_HZ;
  const step = Math.pow(ratio, 1 / bands);
  const low = SPECTRUM_MIN_HZ * Math.pow(step, index);
  return Math.sqrt(low * low * step);
}

/** Display dB for an analyser level on the curve's dB axis. */
export function spectrumDisplayDb(dbfs: number): number {
  if (!Number.isFinite(dbfs)) return -24;
  return Math.min(24, Math.max(-24, dbfs + SPECTRUM_DISPLAY_OFFSET_DB));
}

/** Per-band response used for the thin individual traces. */
export function bandCurve(
  band: ParametricBand,
  points = 120,
  sampleRate = DEFAULT_SAMPLE_RATE,
  dynamicRange = false,
): CurvePoint[] {
  return logFrequencies(points, MIN_FREQ, MAX_FREQ).map((freq) => ({
    freq,
    db: bandResponseDb(band, freq, sampleRate, dynamicRange),
  }));
}
