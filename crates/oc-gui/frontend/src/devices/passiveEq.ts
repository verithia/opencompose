/**
 * Combined response of the passive EQ, mirroring `oc-dsp::passive_eq`.
 * The curve editor draws only through this module.
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

const LOW_BOOST_Q = 1.0;
const LOW_CUT_Q = 0.7;
const HIGH_CUT_Q = 0.707;
const AIR_Q = 0.707;
const TILT_PIVOT_HZ = 1000;

export interface PassiveEqValues {
  lowFreq: number;
  lowBoost: number;
  lowCut: number;
  highFreq: number;
  highBoost: number;
  highQ: number;
  highCutFreq: number;
  highCut: number;
  tilt: number;
  airFreq: number;
  air: number;
}

export const PASSIVE_EQ_DEFAULTS: PassiveEqValues = {
  lowFreq: 100,
  lowBoost: 0,
  lowCut: 0,
  highFreq: 8000,
  highBoost: 0,
  highQ: 1,
  highCutFreq: 10000,
  highCut: 0,
  tilt: 0,
  airFreq: 10000,
  air: 0,
};

export interface PassiveEqStage {
  shape: BiquadShape;
  freq: number;
  q: number;
  gain: number;
}

/** Serial stages in the same order as the DSP, including zero-gain stages. */
export function passiveEqStages(values: PassiveEqValues): PassiveEqStage[] {
  return [
    { shape: "peak", freq: values.lowFreq, q: LOW_BOOST_Q, gain: values.lowBoost },
    { shape: "low_shelf", freq: values.lowFreq, q: LOW_CUT_Q, gain: -values.lowCut },
    { shape: "peak", freq: values.highFreq, q: values.highQ, gain: values.highBoost },
    { shape: "high_shelf", freq: values.highCutFreq, q: HIGH_CUT_Q, gain: -values.highCut },
    { shape: "tilt_shelf", freq: TILT_PIVOT_HZ, q: 1, gain: values.tilt },
    { shape: "high_shelf", freq: values.airFreq, q: AIR_Q, gain: values.air },
  ];
}

export function passiveEqDb(
  values: PassiveEqValues,
  freq: number,
  sampleRate = DEFAULT_SAMPLE_RATE,
): number {
  let total = 0;
  for (const stage of passiveEqStages(values)) {
    if (Math.abs(stage.gain) < 1e-9) continue;
    total += magnitudeDb(
      designBiquad(stage.shape, stage.freq, stage.q, stage.gain, sampleRate),
      freq,
      sampleRate,
    );
  }
  return total;
}

export function passiveEqCurve(
  values: PassiveEqValues,
  points = 180,
  minFreq = MIN_FREQ,
  maxFreq = MAX_FREQ,
): { freq: number; db: number }[] {
  return logFrequencies(points, minFreq, maxFreq).map((freq) => ({
    freq,
    db: passiveEqDb(values, freq),
  }));
}
