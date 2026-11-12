//! Vocal doubler: two to four short modulated delay voices over a mono-safe low end.

use std::f64::consts::TAU;

use super::AudioEffect;
use super::biquad::{BiquadFilter, FilterType};

const MAX_VOICES: usize = 4;
const MIN_VOICES: usize = 2;
const MIN_DELAY_MS: f64 = 8.0;
const MAX_DELAY_MS: f64 = 30.0;
const VOICE_SPACING_MS: f64 = 4.0;
const MIN_FOCUS_HZ: f64 = 200.0;
const MAX_FOCUS_HZ: f64 = 400.0;
const FOCUS_Q: f64 = 0.707;
const MIN_RATE_HZ: f64 = 0.2;
const MAX_RATE_HZ: f64 = 0.5;
const MIN_DEPTH_MS: f64 = 1.0;
const MAX_DEPTH_MS: f64 = 3.0;
const MAX_DETUNE_CENTS: f64 = 20.0;
const MIN_MIX: f64 = 0.0;
const MAX_MIX: f64 = 1.0;
const MAX_DELAY_MS_TOTAL: f64 = 32.0;
const MAX_DRIFT_MS: f64 = 60.0;
const VOICE_RATE_SCALE: [f64; MAX_VOICES] = [1.00, 1.27, 0.84, 1.13];
const VOICE_DEPTH_SCALE: [f64; MAX_VOICES] = [1.00, 0.86, 1.14, 0.92];
const VOICE_PHASE: [f64; MAX_VOICES] = [0.00, 0.37, 0.71, 0.58];

/// A vocal doubler with two to four short modulated delay voices.
pub struct Doubler {
    buffer: Vec<f32>,
    write_pos: usize,
    focus: BiquadFilter,
    phases: [f64; MAX_VOICES],
    phase_inc: [f64; MAX_VOICES],
    depth_samples: [f64; MAX_VOICES],
    base_samples: [f64; MAX_VOICES],
    drift: [f64; MAX_VOICES],
    pan: [f64; MAX_VOICES],
    voices: usize,
    delay_ms: f64,
    rate_hz: f64,
    depth_ms: f64,
    detune_cents: f64,
    focus_hz: f64,
    mix: f64,
    sample_rate: f64,
    voice_gain: f64,
    detune_step: f64,
}

impl Doubler {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1) as f64;
        let max_samples =
            (sample_rate * (MAX_DELAY_MS_TOTAL + MAX_DRIFT_MS) / 1000.0).ceil() as usize + 4;
        let mut doubler = Self {
            buffer: vec![0.0; max_samples],
            write_pos: 0,
            focus: BiquadFilter::new(
                FilterType::HighPass,
                300.0,
                FOCUS_Q,
                0.0,
                sample_rate as u32,
            ),
            phases: VOICE_PHASE,
            phase_inc: [0.0; MAX_VOICES],
            depth_samples: [0.0; MAX_VOICES],
            base_samples: [0.0; MAX_VOICES],
            drift: [0.0; MAX_VOICES],
            pan: [0.0; MAX_VOICES],
            voices: 2,
            delay_ms: 15.0,
            rate_hz: 0.3,
            depth_ms: 2.0,
            detune_cents: 0.0,
            focus_hz: 300.0,
            mix: 0.5,
            sample_rate,
            voice_gain: 0.5,
            detune_step: 0.0,
        };
        doubler.rebuild();
        doubler
    }

    fn rebuild(&mut self) {
        let voices = self.voices as f64;
        let half = (voices - 1.0) * 0.5;
        let spacing = if half > 0.0 {
            VOICE_SPACING_MS
                .min((self.delay_ms - MIN_DELAY_MS) / half)
                .min((MAX_DELAY_MS - self.delay_ms) / half)
        } else {
            0.0
        };
        for index in 0..MAX_VOICES {
            let position = index as f64 - (voices - 1.0) * 0.5;
            self.base_samples[index] = if index < self.voices {
                (self.delay_ms + position * spacing) * 0.001 * self.sample_rate
            } else {
                0.0
            };
            self.phase_inc[index] = if index < self.voices {
                (self.rate_hz * VOICE_RATE_SCALE[index]).clamp(MIN_RATE_HZ, MAX_RATE_HZ)
                    / self.sample_rate
            } else {
                0.0
            };
            self.depth_samples[index] = if index < self.voices {
                (self.depth_ms * VOICE_DEPTH_SCALE[index]).clamp(MIN_DEPTH_MS, MAX_DEPTH_MS)
                    * 0.001
                    * self.sample_rate
            } else {
                0.0
            };
            self.pan[index] = if self.voices <= 1 {
                0.0
            } else {
                -1.0 + 2.0 * index as f64 / (voices - 1.0)
            };
        }
        self.voice_gain = 1.0 / (voices).sqrt();
        let ratio = 2f64.powf(self.detune_cents / 1200.0) - 1.0;
        self.detune_step = ratio * 0.001 * self.sample_rate;
    }

    fn read(&self, delay_samples: f64) -> f64 {
        let len = self.buffer.len();
        let pos = (self.write_pos as f64 + len as f64 - delay_samples) % len as f64;
        let index = pos.floor() as usize;
        let frac = pos - index as f64;
        let next = (index + 1) % len;
        self.buffer[index] as f64 * (1.0 - frac) + self.buffer[next] as f64 * frac
    }
}

impl AudioEffect for Doubler {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let max_drift = MAX_DRIFT_MS * 0.001 * self.sample_rate;
        for frame in buffer.as_chunks_mut::<2>().0 {
            let input_l = frame[0] as f64;
            let input_r = frame[1] as f64;
            let mono = 0.5 * (input_l + input_r);
            let high = self.focus.process_sample(0, mono);
            let low = mono - high;

            self.buffer[self.write_pos] = high as f32;

            let mut wet_l = low;
            let mut wet_r = low;
            for index in 0..self.voices {
                let lfo = (TAU * self.phases[index]).sin();
                if self.detune_cents > 0.0 {
                    let direction = if index % 2 == 0 { -1.0 } else { 1.0 };
                    self.drift[index] = (self.drift[index] + direction * self.detune_step)
                        .clamp(-max_drift, max_drift);
                }
                let delay = (self.base_samples[index]
                    + self.depth_samples[index] * lfo
                    + self.drift[index])
                    .clamp(1.0, self.buffer.len() as f64 - 2.0);
                let voice = self.read(delay) * self.voice_gain;
                let left_gain = 0.5 * (1.0 - self.pan[index]);
                let right_gain = 0.5 * (1.0 + self.pan[index]);
                wet_l += voice * left_gain;
                wet_r += voice * right_gain;
                self.phases[index] += self.phase_inc[index];
                if self.phases[index] >= 1.0 {
                    self.phases[index] -= 1.0;
                }
            }

            let dry = 1.0 - self.mix;
            frame[0] = (dry * input_l + self.mix * wet_l) as f32;
            frame[1] = (dry * input_r + self.mix * wet_r) as f32;
            self.write_pos = (self.write_pos + 1) % self.buffer.len();
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "voices" => {
                self.voices = value.round().clamp(MIN_VOICES as f64, MAX_VOICES as f64) as usize;
                self.rebuild();
            }
            "delay" => {
                self.delay_ms = value.clamp(MIN_DELAY_MS, MAX_DELAY_MS);
                self.rebuild();
            }
            "rate" => {
                self.rate_hz = value.clamp(MIN_RATE_HZ, MAX_RATE_HZ);
                self.rebuild();
            }
            "depth" => {
                self.depth_ms = value.clamp(MIN_DEPTH_MS, MAX_DEPTH_MS);
                self.rebuild();
            }
            "detune" => {
                let next = value.clamp(0.0, MAX_DETUNE_CENTS);
                if (next - self.detune_cents).abs() > f64::EPSILON {
                    self.drift = [0.0; MAX_VOICES];
                }
                self.detune_cents = next;
                self.rebuild();
            }
            "focus" => {
                self.focus_hz = value.clamp(MIN_FOCUS_HZ, MAX_FOCUS_HZ);
                self.focus.set_params(self.focus_hz, FOCUS_Q, 0.0);
            }
            "mix" => self.mix = value.clamp(MIN_MIX, MAX_MIX),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "voices" => Some(self.voices as f64),
            "delay" => Some(self.delay_ms),
            "rate" => Some(self.rate_hz),
            "depth" => Some(self.depth_ms),
            "detune" => Some(self.detune_cents),
            "focus" => Some(self.focus_hz),
            "mix" => Some(self.mix),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.write_pos = 0;
        self.focus.reset();
        self.phases = VOICE_PHASE;
        self.drift = [0.0; MAX_VOICES];
    }

    fn name(&self) -> &str {
        "doubler"
    }

    fn latency_samples(&self) -> u32 {
        0
    }

    fn tail_samples(&self) -> u32 {
        ((MAX_DELAY_MS_TOTAL + MAX_DRIFT_MS) * 0.001 * self.sample_rate) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(freq: f64, amplitude: f64, seconds: f64) -> Vec<f32> {
        let frames = (RATE as f64 * seconds) as usize;
        let mut buffer = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let value = (amplitude * (TAU * freq * index as f64 / RATE as f64).sin()) as f32;
            buffer.push(value);
            buffer.push(value);
        }
        buffer
    }

    fn noise(seconds: f64) -> Vec<f32> {
        let frames = (RATE as f64 * seconds) as usize;
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut buffer = Vec::with_capacity(frames * 2);
        for _ in 0..frames {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let left = ((state >> 40) as f64 / (1u64 << 24) as f64 * 2.0 - 1.0) * 0.25;
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let right = ((state >> 40) as f64 / (1u64 << 24) as f64 * 2.0 - 1.0) * 0.25;
            buffer.push(left as f32);
            buffer.push(right as f32);
        }
        buffer
    }

    fn band_rms(samples: &[f32], channel: usize, seconds: f64) -> f64 {
        let start = (samples.len() / 2).saturating_sub((RATE as f64 * seconds) as usize);
        let mut high = BiquadFilter::new(FilterType::HighPass, 100.0, 0.707, 0.0, RATE);
        let mut low = BiquadFilter::new(FilterType::LowPass, 300.0, 0.707, 0.0, RATE);
        let mut sum = 0.0;
        let mut count = 0;
        for frame in start..samples.len() / 2 {
            let mut value = samples[frame * 2 + channel] as f64;
            value = high.process_sample(0, value);
            value = low.process_sample(0, value);
            sum += value * value;
            count += 1;
        }
        (sum / count.max(1) as f64).sqrt()
    }

    fn zero_lag_correlation(a: &[f32], b: &[f32]) -> f64 {
        let mut dot = 0.0;
        let mut aa = 0.0;
        let mut bb = 0.0;
        for (x, y) in a.iter().zip(b) {
            dot += *x as f64 * *y as f64;
            aa += *x as f64 * *x as f64;
            bb += *y as f64 * *y as f64;
        }
        dot / (aa * bb).sqrt().max(1e-30)
    }

    fn frequency_from_crossings(samples: &[f32], seconds: f64) -> f64 {
        let start = (samples.len() / 2).saturating_sub((RATE as f64 * seconds) as usize);
        let mut crossings = 0;
        let mut previous = samples[start * 2] as f64;
        for frame in start..samples.len() / 2 {
            let value = samples[frame * 2] as f64;
            if previous <= 0.0 && value > 0.0 {
                crossings += 1;
            }
            previous = value;
        }
        crossings as f64 / seconds
    }

    fn make() -> Doubler {
        let mut doubler = Doubler::new(RATE);
        doubler.set_param("mix", 1.0);
        doubler
    }

    #[test]
    fn mono_summing_the_wet_path_keeps_the_low_band_within_one_db() {
        let input = noise(2.5);
        let mut wet = input.clone();
        make().process(&mut wet, RATE);

        let left = band_rms(&wet, 0, 1.5);
        let right = band_rms(&wet, 1, 1.5);
        let mono: Vec<f32> = wet
            .chunks_exact(2)
            .flat_map(|frame| [(frame[0] + frame[1]) * 0.5; 2])
            .collect();
        let mono_band = band_rms(&mono, 0, 1.5);
        let stereo_energy = 0.5 * (left * left + right * right);
        let change_db = 10.0 * (mono_band * mono_band / stereo_energy.max(1e-30)).log10();
        assert!(
            change_db.abs() <= 1.0,
            "mono sum moved the 100-300 Hz band by {change_db:.2} dB"
        );
    }

    #[test]
    fn two_default_voices_are_decorrelated_from_the_dry_signal() {
        let input = noise(2.5);
        let mut wet = input.clone();
        make().process(&mut wet, RATE);

        let mut dry_high = BiquadFilter::new(FilterType::HighPass, 300.0, 0.707, 0.0, RATE);
        let mut wet_high = BiquadFilter::new(FilterType::HighPass, 300.0, 0.707, 0.0, RATE);
        let start = (RATE as usize) / 2;
        let mut dry = Vec::new();
        let mut processed = Vec::new();
        for frame in start..input.len() / 2 {
            let d = dry_high.process_sample(0, input[frame * 2] as f64);
            let w = wet_high.process_sample(0, wet[frame * 2] as f64);
            dry.push(d as f32);
            processed.push(w as f32);
        }
        let correlation = zero_lag_correlation(&dry, &processed);
        assert!(
            correlation.abs() < 0.3,
            "zero-lag correlation was {correlation:.3}"
        );
    }

    #[test]
    fn one_voice_keeps_a_one_khz_tone_within_twenty_cents() {
        let input = tone(1_000.0, 0.2, 2.0);
        let mut wet = input.clone();
        let mut doubler = make();
        doubler.set_param("voices", 2.0);
        doubler.set_param("detune", 0.0);
        doubler.process(&mut wet, RATE);

        let frequency = frequency_from_crossings(&wet, 1.0);
        let cents = 1200.0 * (frequency / 1_000.0).log2();
        assert!(
            cents.abs() <= 20.0,
            "pitch moved {cents:.1} cents at {frequency:.2} Hz"
        );
    }

    #[test]
    fn the_voice_lfos_stay_inside_the_stated_rate_and_depth_ranges() {
        let mut doubler = Doubler::new(RATE);
        doubler.set_param("voices", 4.0);
        doubler.set_param("rate", 0.5);
        doubler.set_param("depth", 3.0);
        for index in 0..4 {
            let rate = doubler.phase_inc[index] * doubler.sample_rate;
            let depth = doubler.depth_samples[index] / doubler.sample_rate * 1000.0;
            assert!((0.2..=0.5).contains(&rate), "voice {index} rate {rate}");
            assert!((1.0..=3.0).contains(&depth), "voice {index} depth {depth}");
        }
    }

    #[test]
    fn zero_mix_is_an_exact_bypass_and_reports_zero_latency() {
        let input = tone(440.0, 0.3, 0.5);
        let mut output = input.clone();
        let mut doubler = Doubler::new(RATE);
        doubler.set_param("mix", 0.0);
        doubler.process(&mut output, RATE);
        assert_eq!(output, input);
        assert_eq!(doubler.latency_samples(), 0);
    }

    #[test]
    #[ignore]
    fn measurements_for_docs() {
        let input = noise(2.5);
        let mut wet = input.clone();
        make().process(&mut wet, RATE);
        let left = band_rms(&wet, 0, 1.5);
        let right = band_rms(&wet, 1, 1.5);
        let mono: Vec<f32> = wet
            .chunks_exact(2)
            .flat_map(|frame| [(frame[0] + frame[1]) * 0.5; 2])
            .collect();
        let mono_band = band_rms(&mono, 0, 1.5);
        let stereo_energy = 0.5 * (left * left + right * right);
        let change_db = 10.0 * (mono_band * mono_band / stereo_energy.max(1e-30)).log10();
        println!("doubler mono sum, 100-300 Hz: {change_db:.3} dB");

        let mut dry_high = BiquadFilter::new(FilterType::HighPass, 300.0, 0.707, 0.0, RATE);
        let mut wet_high = BiquadFilter::new(FilterType::HighPass, 300.0, 0.707, 0.0, RATE);
        let start = (RATE as usize) / 2;
        let mut dry = Vec::new();
        let mut processed = Vec::new();
        for frame in start..input.len() / 2 {
            dry.push(dry_high.process_sample(0, input[frame * 2] as f64) as f32);
            processed.push(wet_high.process_sample(0, wet[frame * 2] as f64) as f32);
        }
        println!(
            "doubler zero-lag correlation vs dry: {:.3}",
            zero_lag_correlation(&dry, &processed)
        );

        let input = tone(1_000.0, 0.2, 2.0);
        let mut wet = input.clone();
        make().process(&mut wet, RATE);
        let frequency = frequency_from_crossings(&wet, 1.0);
        println!(
            "doubler 1 kHz pitch: {frequency:.2} Hz ({:.1} cents)",
            1200.0 * (frequency / 1_000.0).log2()
        );

        for voices in [2.0, 3.0, 4.0] {
            let mut doubler = Doubler::new(RATE);
            doubler.set_param("voices", voices);
            let delays: Vec<f64> = (0..doubler.voices)
                .map(|index| doubler.base_samples[index] / doubler.sample_rate * 1000.0)
                .collect();
            let rates: Vec<f64> = (0..doubler.voices)
                .map(|index| doubler.phase_inc[index] * doubler.sample_rate)
                .collect();
            println!(
                "doubler {voices:.0} voices: delays {:?} ms, rates {:?} Hz",
                delays
                    .iter()
                    .map(|value| (value * 10.0).round() / 10.0)
                    .collect::<Vec<_>>(),
                rates
                    .iter()
                    .map(|value| (value * 100.0).round() / 100.0)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn the_focus_high_pass_makes_the_low_end_mono_by_construction() {
        let left = tone(150.0, 0.3, 1.5);
        let mut stereo = left.clone();
        for frame in stereo.chunks_exact_mut(2) {
            frame[1] *= 0.2;
        }
        let mut wet = stereo.clone();
        make().process(&mut wet, RATE);
        let mono: Vec<f32> = wet
            .chunks_exact(2)
            .flat_map(|frame| [(frame[0] + frame[1]) * 0.5; 2])
            .collect();
        let left_band = band_rms(&wet, 0, 1.0);
        let right_band = band_rms(&wet, 1, 1.0);
        let mono_band = band_rms(&mono, 0, 1.0);
        let channel_db = 20.0 * (left_band / right_band.max(1e-12)).log10();
        let mono_db = 20.0 * (mono_band / left_band.max(1e-12)).log10();
        assert!(
            channel_db.abs() < 1.0,
            "wet low end differs by {channel_db:.2} dB"
        );
        assert!(
            mono_db.abs() < 1.0,
            "mono sum changed the low band by {mono_db:.2} dB"
        );
    }
}
