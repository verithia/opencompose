/**
 * Biquad design and magnitude response mirroring `oc-dsp::biquad`; the curve editors draw only through it.
 */

export type BiquadShape =
  | "low_pass"
  | "high_pass"
  | "band_pass"
  | "notch"
  | "peak"
  | "low_shelf"
  | "high_shelf"
  | "tilt_shelf";

export const DEFAULT_SAMPLE_RATE = 48000;
export const MIN_FREQ = 20;
export const MAX_FREQ = 20000;

export interface BiquadCoefficients {
  b0: number;
  b1: number;
  b2: number;
  a1: number;
  a2: number;
}

/** Normalized (a0 = 1) coefficients, clamped exactly like the Rust helper. */
export function designBiquad(
  shape: BiquadShape,
  freq: number,
  q: number,
  gainDb: number,
  sampleRate = DEFAULT_SAMPLE_RATE,
): BiquadCoefficients {
  const clampedFreq = clamp(freq, MIN_FREQ, sampleRate * 0.45);
  const clampedQ = Math.max(q, 0.1);
  const w0 = (2 * Math.PI * clampedFreq) / sampleRate;
  const cosW0 = Math.cos(w0);
  const sinW0 = Math.sin(w0);
  const alpha = sinW0 / (2 * clampedQ);
  const a = Math.pow(10, gainDb / 40);

  let b0: number;
  let b1: number;
  let b2: number;
  let a0: number;
  let a1: number;
  let a2: number;
  switch (shape) {
    case "low_pass":
      b0 = (1 - cosW0) / 2;
      b1 = 1 - cosW0;
      b2 = (1 - cosW0) / 2;
      a0 = 1 + alpha;
      a1 = -2 * cosW0;
      a2 = 1 - alpha;
      break;
    case "high_pass":
      b0 = (1 + cosW0) / 2;
      b1 = -(1 + cosW0);
      b2 = (1 + cosW0) / 2;
      a0 = 1 + alpha;
      a1 = -2 * cosW0;
      a2 = 1 - alpha;
      break;
    case "band_pass":
      b0 = alpha;
      b1 = 0;
      b2 = -alpha;
      a0 = 1 + alpha;
      a1 = -2 * cosW0;
      a2 = 1 - alpha;
      break;
    case "notch":
      b0 = 1;
      b1 = -2 * cosW0;
      b2 = 1;
      a0 = 1 + alpha;
      a1 = -2 * cosW0;
      a2 = 1 - alpha;
      break;
    case "peak": {
      const alphaA = alpha * a;
      const alphaDivA = alpha / a;
      b0 = 1 + alphaA;
      b1 = -2 * cosW0;
      b2 = 1 - alphaA;
      a0 = 1 + alphaDivA;
      a1 = -2 * cosW0;
      a2 = 1 - alphaDivA;
      break;
    }
    case "low_shelf": {
      const twoSqrtAAlpha = 2 * Math.sqrt(a) * alpha;
      b0 = a * (a + 1 - (a - 1) * cosW0 + twoSqrtAAlpha);
      b1 = 2 * a * (a - 1 - (a + 1) * cosW0);
      b2 = a * (a + 1 - (a - 1) * cosW0 - twoSqrtAAlpha);
      a0 = a + 1 + (a - 1) * cosW0 + twoSqrtAAlpha;
      a1 = -2 * (a - 1 + (a + 1) * cosW0);
      a2 = a + 1 + (a - 1) * cosW0 - twoSqrtAAlpha;
      break;
    }
    case "high_shelf": {
      const twoSqrtAAlpha = 2 * Math.sqrt(a) * alpha;
      b0 = a * (a + 1 + (a - 1) * cosW0 + twoSqrtAAlpha);
      b1 = -2 * a * (a - 1 + (a + 1) * cosW0);
      b2 = a * (a + 1 + (a - 1) * cosW0 - twoSqrtAAlpha);
      a0 = a + 1 - (a - 1) * cosW0 + twoSqrtAAlpha;
      a1 = 2 * (a - 1 - (a + 1) * cosW0);
      a2 = a + 1 - (a - 1) * cosW0 - twoSqrtAAlpha;
      break;
    }
    case "tilt_shelf": {
      const centring = Math.pow(10, -gainDb / 40);
      const low = Math.tan((w0 * centring) / 2);
      const high = Math.tan(w0 / centring / 2);
      const scale = (centring * high) / low;
      b0 = (scale * (1 + low)) / (1 + high);
      b1 = (scale * (low - 1)) / (1 + high);
      b2 = 0;
      a0 = 1;
      a1 = (high - 1) / (1 + high);
      a2 = 0;
      break;
    }
  }
  return { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 };
}

/** Magnitude at `freqHz`, in dB; mirrors `BiquadCoefficients::response_db`. */
export function magnitudeDb(
  coefficients: BiquadCoefficients,
  freqHz: number,
  sampleRate = DEFAULT_SAMPLE_RATE,
): number {
  const w = (2 * Math.PI * clamp(freqHz, 0, sampleRate * 0.5)) / sampleRate;
  const sinW = Math.sin(w);
  const cosW = Math.cos(w);
  const cos2W = Math.cos(2 * w);
  const sin2W = Math.sin(2 * w);
  const numRe = coefficients.b0 + coefficients.b1 * cosW + coefficients.b2 * cos2W;
  const numIm = -(coefficients.b1 * sinW + coefficients.b2 * sin2W);
  const denRe = 1 + coefficients.a1 * cosW + coefficients.a2 * cos2W;
  const denIm = -(coefficients.a1 * sinW + coefficients.a2 * sin2W);
  const numerator = Math.hypot(numRe, numIm);
  const denominator = Math.hypot(denRe, denIm);
  if (denominator <= Number.MIN_VALUE || !Number.isFinite(numerator)) return 0;
  return 20 * Math.log10(numerator / denominator);
}

/** Log-spaced probe frequencies, inclusive of both ends. */
export function logFrequencies(
  points: number,
  minFreq = MIN_FREQ,
  maxFreq = MAX_FREQ,
): number[] {
  const values: number[] = [];
  for (let index = 0; index < points; index += 1) {
    const t = points === 1 ? 0 : index / (points - 1);
    values.push(minFreq * Math.pow(maxFreq / minFreq, t));
  }
  return values;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}
