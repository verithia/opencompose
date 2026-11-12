/**
 * Summed response for the fixed 3-band EQ view; delegates to the shared biquad module.
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

export type EqBandId = "low" | "mid" | "high";

export interface EqBandValue {
  id: EqBandId;
  freq: number;
  gain: number;
  q: number;
}

const SHELF_Q = 0.707;

function bandShape(band: EqBandValue): BiquadShape {
  return band.id === "low" ? "low_shelf" : band.id === "high" ? "high_shelf" : "peak";
}

function bandQ(band: EqBandValue): number {
  return band.id === "mid" ? band.q : SHELF_Q;
}

export function eqMagnitudeDb(
  bands: EqBandValue[],
  freq: number,
  sampleRate = DEFAULT_SAMPLE_RATE,
): number {
  let total = 0;
  for (const band of bands) {
    if (Math.abs(band.gain) < 1e-6) continue;
    const coefficients = designBiquad(bandShape(band), band.freq, bandQ(band), band.gain, sampleRate);
    total += magnitudeDb(coefficients, freq, sampleRate);
  }
  return total;
}

export function eqCurve(
  bands: EqBandValue[],
  points = 160,
  minFreq = MIN_FREQ,
  maxFreq = MAX_FREQ,
): { freq: number; db: number }[] {
  const coefficients = bands
    .filter((band) => Math.abs(band.gain) >= 1e-6)
    .map((band) => designBiquad(bandShape(band), band.freq, bandQ(band), band.gain));
  return logFrequencies(points, minFreq, maxFreq).map((freq) => ({
    freq,
    db: coefficients.reduce((total, entry) => total + magnitudeDb(entry, freq), 0),
  }));
}
