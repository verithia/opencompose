//! Three-band RMS compander with per-band gain and a lookahead limiter.
//!
//! `new` allocates; `process_frame`, `set_param` and `reset` do not allocate,
//! lock or spawn.

use super::crossover::{MAX_BANDS, MultibandCrossover, PhaseMode, Slope};
use super::detector::db_to_linear;

/// Number of compander bands.
pub const BANDS: usize = 3;
/// Default low-band crossover frequency.
pub const DEFAULT_CROSSOVER_LOW_HZ: f64 = 120.0;
/// Default high-band crossover frequency.
pub const DEFAULT_CROSSOVER_HIGH_HZ: f64 = 2_500.0;

/// Lookahead delay of the output limiter, in milliseconds.
pub const LIMITER_LOOKAHEAD_MS: f64 = 2.0;
/// Sample-peak ceiling of the output limiter, in dBFS.
pub const LIMITER_CEILING_DB: f64 = -0.3;
/// Release time of the limiter envelope.
const LIMITER_RELEASE_MS: f64 = 80.0;
/// Sine crest factor: a sine's peak-equivalent level is its RMS level + 3.01 dB.
pub const SINE_CREST_DB: f64 = 3.010_299_956_639_812;

const MIN_THRESHOLD_DB: f64 = -60.0;
const MAX_THRESHOLD_DB: f64 = 20.0;
const MIN_RATIO: f64 = 1.0;
const MAX_RATIO: f64 = 1_000.0;
const MIN_ATTACK_MS: f64 = 0.1;
const MAX_ATTACK_MS: f64 = 200.0;
const MIN_RELEASE_MS: f64 = 5.0;
const MAX_RELEASE_MS: f64 = 2_000.0;
const MIN_BAND_GAIN_DB: f64 = -24.0;
const MAX_BAND_GAIN_DB: f64 = 24.0;
const MIN_MAKEUP_DB: f64 = -24.0;
const MAX_MAKEUP_DB: f64 = 24.0;
/// Deepest attenuation a band may apply.
const MAX_COMPRESSION_DB: f64 = 60.0;
/// Largest boost the below-threshold expansion may apply.
const MAX_EXPANSION_DB: f64 = 36.0;
const MIN_POWER: f64 = 1e-30;

/// Lookahead delay in samples for a limiter at `sample_rate`.
pub fn lookahead_samples(sample_rate: f64) -> usize {
    (LIMITER_LOOKAHEAD_MS * 0.001 * sample_rate.max(8_000.0)).round() as usize
}

#[derive(Debug, Clone, Copy)]
struct Band {
    threshold_db: f64,
    ratio: f64,
    threshold_below_db: f64,
    ratio_below: f64,
    gain_db: f64,
    attack_ms: f64,
    release_ms: f64,
    attack_coefficient: f64,
    release_coefficient: f64,
    power: f64,
    last_gain_db: f64,
}

impl Band {
    fn new(sample_rate: u32) -> Self {
        let mut band = Self {
            threshold_db: -18.0,
            ratio: 4.0,
            threshold_below_db: -30.0,
            ratio_below: 1.0,
            gain_db: 0.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            attack_coefficient: 0.0,
            release_coefficient: 0.0,
            power: 0.0,
            last_gain_db: 0.0,
        };
        band.update_times(sample_rate);
        band
    }

    fn update_times(&mut self, sample_rate: u32) {
        let rate = sample_rate.max(8_000) as f64;
        self.attack_coefficient = time_coefficient(self.attack_ms, rate);
        self.release_coefficient = time_coefficient(self.release_ms, rate);
    }

    fn reset(&mut self) {
        self.power = 0.0;
        self.last_gain_db = 0.0;
    }

    /// Static gain change in dB for one detected level. Positive is a boost.
    fn static_gain_db(&self, level_db: f64) -> f64 {
        let mut gain = self.gain_db;
        if level_db > self.threshold_db {
            gain -= (1.0 - 1.0 / self.ratio) * (level_db - self.threshold_db);
        } else if self.ratio_below > 1.0 && level_db < self.threshold_below_db {
            gain += (1.0 - 1.0 / self.ratio_below) * (self.threshold_below_db - level_db);
        }
        gain.clamp(-MAX_COMPRESSION_DB, MAX_EXPANSION_DB)
    }

    /// One mean-square detector step and its band gain change in dB.
    fn process(&mut self, left: f64, right: f64) -> f64 {
        let power = 0.5 * (left * left + right * right);
        let coefficient = if power > self.power {
            self.attack_coefficient
        } else {
            self.release_coefficient
        };
        self.power = coefficient * self.power + (1.0 - coefficient) * power;
        let level_db = 10.0 * self.power.max(MIN_POWER).log10();
        let gain_db = self.static_gain_db(level_db);
        self.last_gain_db = gain_db;
        gain_db
    }
}

fn time_coefficient(ms: f64, sample_rate: f64) -> f64 {
    (-1.0 / (ms.max(0.01) * 0.001 * sample_rate)).exp()
}

/// Lookahead limiter: delayed signal plus a monotonic window of required gains.
struct LookaheadLimiter {
    delay_left: Vec<f64>,
    delay_right: Vec<f64>,
    position: usize,
    frame: u64,
    window: std::collections::VecDeque<(u64, f64)>,
    gain: f64,
    release_coefficient: f64,
    ceiling: f64,
}

impl LookaheadLimiter {
    fn new(sample_rate: u32) -> Self {
        let length = lookahead_samples(sample_rate as f64);
        Self {
            delay_left: vec![0.0; length.max(1)],
            delay_right: vec![0.0; length.max(1)],
            position: 0,
            frame: 0,
            window: std::collections::VecDeque::with_capacity(length + 1),
            gain: 1.0,
            release_coefficient: time_coefficient(LIMITER_RELEASE_MS, sample_rate as f64),
            ceiling: db_to_linear(LIMITER_CEILING_DB),
        }
    }

    fn reset(&mut self) {
        self.delay_left.fill(0.0);
        self.delay_right.fill(0.0);
        self.position = 0;
        self.frame = 0;
        self.window.clear();
        self.gain = 1.0;
    }

    fn process(&mut self, left: f64, right: f64) -> (f64, f64) {
        let length = self.delay_left.len();
        if length == 0 {
            return (left, right);
        }
        let delayed_left = self.delay_left[self.position];
        let delayed_right = self.delay_right[self.position];
        self.delay_left[self.position] = left;
        self.delay_right[self.position] = right;
        self.position = (self.position + 1) % length;

        let peak = left.abs().max(right.abs());
        let target = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };
        let oldest = self.frame.saturating_sub(length as u64);
        while self
            .window
            .front()
            .is_some_and(|(frame, _)| *frame < oldest)
        {
            self.window.pop_front();
        }
        while self
            .window
            .back()
            .is_some_and(|(_, value)| *value >= target)
        {
            self.window.pop_back();
        }
        self.window.push_back((self.frame, target));
        self.frame += 1;
        let required = self.window.front().map_or(1.0, |(_, value)| *value);

        let released = 1.0 - (1.0 - self.gain) * self.release_coefficient;
        self.gain = required.min(released);
        (delayed_left * self.gain, delayed_right * self.gain)
    }
}

/// Three-band RMS compander with a lookahead limiter at its output.
pub struct Compander {
    crossover: MultibandCrossover,
    bands: [Band; BANDS],
    crossover_freqs: [f64; MAX_BANDS - 1],
    makeup_db: f64,
    mix: f64,
    limiter: LookaheadLimiter,
    limiter_enabled: bool,
    sample_rate: u32,
}

impl Compander {
    /// A three-band compander at the default crossovers with the limiter on.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(8_000);
        let mut compander = Self {
            crossover: MultibandCrossover::new(sample_rate),
            bands: [Band::new(sample_rate); BANDS],
            crossover_freqs: [DEFAULT_CROSSOVER_LOW_HZ, DEFAULT_CROSSOVER_HIGH_HZ, 0.0],
            makeup_db: 0.0,
            mix: 1.0,
            limiter: LookaheadLimiter::new(sample_rate),
            limiter_enabled: true,
            sample_rate,
        };
        compander.configure_crossover();
        compander
    }

    fn configure_crossover(&mut self) {
        self.crossover.configure(
            BANDS,
            self.crossover_freqs,
            Slope::Oct12,
            PhaseMode::Minimum,
        );
    }

    fn set_all_bands(&mut self, update: impl Fn(&mut Band)) {
        for band in &mut self.bands {
            update(band);
        }
    }

    /// Set one named parameter, ignoring non-finite values.
    pub fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "crossover_low" => {
                let frequency = value.clamp(20.0, self.sample_rate as f64 * 0.45);
                if (frequency - self.crossover_freqs[0]).abs() > 1e-9 {
                    self.crossover_freqs[0] = frequency;
                    self.configure_crossover();
                }
            }
            "crossover_high" => {
                let frequency = value.clamp(20.0, self.sample_rate as f64 * 0.45);
                if (frequency - self.crossover_freqs[1]).abs() > 1e-9 {
                    self.crossover_freqs[1] = frequency;
                    self.configure_crossover();
                }
            }
            "threshold" => {
                let threshold = value.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB);
                self.set_all_bands(|band| {
                    band.threshold_db = threshold;
                    band.threshold_below_db = band.threshold_below_db.min(threshold);
                });
            }
            "threshold_below" => {
                let threshold = value.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB);
                self.set_all_bands(|band| {
                    band.threshold_below_db = threshold.min(band.threshold_db);
                });
            }
            "ratio" => {
                let ratio = value.clamp(MIN_RATIO, MAX_RATIO);
                self.set_all_bands(|band| band.ratio = ratio);
            }
            "ratio_below" => {
                let ratio = value.clamp(MIN_RATIO, MAX_RATIO);
                self.set_all_bands(|band| band.ratio_below = ratio);
            }
            "attack" => {
                let attack = value.clamp(MIN_ATTACK_MS, MAX_ATTACK_MS);
                let sample_rate = self.sample_rate;
                self.set_all_bands(|band| {
                    band.attack_ms = attack;
                    band.update_times(sample_rate);
                });
            }
            "release" => {
                let release = value.clamp(MIN_RELEASE_MS, MAX_RELEASE_MS);
                let sample_rate = self.sample_rate;
                self.set_all_bands(|band| {
                    band.release_ms = release;
                    band.update_times(sample_rate);
                });
            }
            "band1_gain" => self.set_band_gain(0, value),
            "band2_gain" => self.set_band_gain(1, value),
            "band3_gain" => self.set_band_gain(2, value),
            "makeup" => self.makeup_db = value.clamp(MIN_MAKEUP_DB, MAX_MAKEUP_DB),
            "mix" => self.mix = value.clamp(0.0, 1.0),
            "limiter" => {
                let enabled = value >= 0.5;
                if enabled != self.limiter_enabled {
                    self.limiter_enabled = enabled;
                    self.limiter.reset();
                }
            }
            _ => {}
        }
    }

    fn set_band_gain(&mut self, band: usize, value: f64) {
        if let Some(band) = self.bands.get_mut(band) {
            band.gain_db = value.clamp(MIN_BAND_GAIN_DB, MAX_BAND_GAIN_DB);
        }
    }

    /// Process one stereo frame. Never allocates.
    pub fn process_frame(&mut self, left: f64, right: f64) -> (f64, f64) {
        let split = self.crossover.process_frame(left, right);
        let mut wet_left = 0.0;
        let mut wet_right = 0.0;
        for (band, split) in self.bands.iter_mut().zip(split.iter()) {
            let gain_db = band.process(split[0], split[1]);
            let gain = db_to_linear(gain_db);
            wet_left += split[0] * gain;
            wet_right += split[1] * gain;
        }
        let makeup = db_to_linear(self.makeup_db);
        wet_left *= makeup;
        wet_right *= makeup;

        let mix = self.mix;
        let mixed_left = left + (wet_left - left) * mix;
        let mixed_right = right + (wet_right - right) * mix;
        if self.limiter_enabled {
            self.limiter.process(mixed_left, mixed_right)
        } else {
            (mixed_left, mixed_right)
        }
    }

    /// Clear every band, the crossover and the limiter.
    pub fn reset(&mut self) {
        self.crossover.reset();
        for band in &mut self.bands {
            band.reset();
        }
        self.limiter.reset();
    }

    /// Module latency in samples: the limiter's lookahead when it is enabled.
    pub fn latency_samples(&self) -> usize {
        if self.limiter_enabled {
            self.limiter.delay_left.len()
        } else {
            0
        }
    }

    /// Last gain change of one band in dB, including its static gain.
    pub fn band_gain_db(&self, band: usize) -> Option<f64> {
        self.bands.get(band).map(|band| band.last_gain_db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SR: u32 = 48_000;

    fn sine(freq: f64, amplitude: f64, frames: usize) -> Vec<f64> {
        (0..frames)
            .map(|frame| amplitude * (TAU * freq * frame as f64 / SR as f64).sin())
            .collect()
    }

    fn rms(samples: &[f64]) -> f64 {
        (samples.iter().map(|value| value * value).sum::<f64>() / samples.len() as f64).sqrt()
    }

    fn level_db(samples: &[f64], start: usize) -> f64 {
        20.0 * rms(&samples[start..]).max(1e-30).log10()
    }

    fn quiet_compander() -> Compander {
        let mut compander = Compander::new(SR);
        compander.set_param("threshold", 20.0);
        compander.set_param("ratio", 1.0);
        compander.set_param("threshold_below", -60.0);
        compander.set_param("ratio_below", 1.0);
        compander.set_param("mix", 1.0);
        compander.set_param("limiter", 0.0);
        compander
    }

    fn render(compander: &mut Compander, input: &[f64]) -> Vec<f64> {
        input
            .iter()
            .map(|sample| compander.process_frame(*sample, *sample).0)
            .collect()
    }

    fn measured_band_gain(
        compander: &mut Compander,
        band: usize,
        frequency: f64,
        rms_db: f64,
    ) -> (f64, f64) {
        let amplitude = std::f64::consts::SQRT_2 * 10f64.powf(rms_db / 20.0);
        let frames = 48_000;
        let input = sine(frequency, amplitude, frames);
        for sample in &input {
            compander.process_frame(*sample, *sample);
        }
        let detected = 10.0 * compander.bands[band].power.max(1e-30).log10();
        (compander.band_gain_db(band).unwrap_or(0.0), detected)
    }

    #[test]
    fn band_sum_is_flat_within_a_tenth_of_a_db() {
        let mut compander = quiet_compander();
        for frequency in [60.0, 250.0, 900.0, 2_500.0, 9_000.0, 16_000.0] {
            let frames = 48_000;
            let input = sine(frequency, 0.2, frames);
            let output = render(&mut compander, &input);
            let deviation = level_db(&output, frames / 2) - level_db(&input, frames / 2);
            assert!(
                deviation.abs() <= 0.1,
                "{frequency} Hz band sum deviated {deviation:.3} dB"
            );
            compander.reset();
        }
    }

    #[test]
    fn rms_detector_follows_the_static_curve_in_every_band() {
        let mut compander = Compander::new(SR);
        compander.set_param("threshold", -20.0);
        compander.set_param("ratio", 4.0);
        compander.set_param("threshold_below", -60.0);
        compander.set_param("ratio_below", 1.0);
        compander.set_param("attack", 1.0);
        compander.set_param("release", 50.0);
        compander.set_param("limiter", 0.0);
        for (band, frequency) in [(0, 60.0), (1, 700.0), (2, 8_000.0)] {
            for level in [-40.0, -30.0, -10.0, 0.0] {
                let (gain, detected) = measured_band_gain(&mut compander, band, frequency, level);
                assert!(
                    (detected - level).abs() < 2.5,
                    "band {band} at {level} dB detected {detected:.2} dB"
                );
                let expected = if detected > -20.0 {
                    -(1.0 - 1.0 / 4.0) * (detected + 20.0)
                } else {
                    0.0
                };
                assert!(
                    (gain - expected).abs() < 0.25,
                    "band {band} at {level} dB: gain {gain:.3} vs {expected:.3}"
                );
            }
        }
    }

    #[test]
    fn below_threshold_expansion_is_upward() {
        let mut compander = Compander::new(SR);
        compander.set_param("threshold", 0.0);
        compander.set_param("ratio", 4.0);
        compander.set_param("threshold_below", -30.0);
        compander.set_param("ratio_below", 2.0);
        compander.set_param("attack", 1.0);
        compander.set_param("release", 50.0);
        compander.set_param("limiter", 0.0);
        for level in [-40.0, -30.0, -20.0, 6.0] {
            let (gain, detected) = measured_band_gain(&mut compander, 0, 60.0, level);
            assert!(
                (detected - level).abs() < 2.5,
                "expander probe at {level} dB detected {detected:.2} dB"
            );
            let expected = if detected < -30.0 {
                0.5 * (-30.0 - detected)
            } else if detected > 0.0 {
                -0.75 * detected
            } else {
                0.0
            };
            assert!(
                (gain - expected).abs() < 0.3,
                "expander at {level} dB: gain {gain:.3} vs {expected:.3}"
            );
        }
    }

    #[test]
    fn lookahead_limiter_delays_by_its_reported_latency_and_holds_the_ceiling() {
        let mut compander = quiet_compander();
        compander.set_param("mix", 0.0);
        compander.set_param("limiter", 1.0);
        let latency = compander.latency_samples();
        assert_eq!(latency, lookahead_samples(SR as f64));
        assert_eq!(latency, 96);

        let frames = 1_000;
        let mut input = vec![0.0; frames];
        input[100] = 2.0;
        let output = render(&mut compander, &input);
        let peak = output
            .iter()
            .fold(0.0_f64, |peak, value| peak.max(value.abs()));
        assert!(
            peak <= db_to_linear(LIMITER_CEILING_DB) * 1.000_001,
            "limiter peak {peak:.6} exceeded the ceiling"
        );
        assert!(output[100 + latency].abs() > 0.9);
        assert!(
            output[..100 + latency]
                .iter()
                .all(|value| value.abs() < 1e-12)
        );

        compander.reset();
        let mut below = vec![0.0; frames];
        below[100] = 0.1;
        let output = render(&mut compander, &below);
        assert!((output[100 + latency] - 0.1).abs() < 1e-12);
    }

    #[test]
    fn zero_gain_reduction_is_unit_magnitude() {
        let mut compander = Compander::new(SR);
        compander.set_param("threshold", 0.0);
        compander.set_param("ratio", 4.0);
        compander.set_param("ratio_below", 1.0);
        compander.set_param("limiter", 1.0);
        let frames = 48_000;
        let input = sine(440.0, 0.02, frames);
        let mut output = Vec::with_capacity(frames);
        for sample in &input {
            output.push(compander.process_frame(*sample, *sample).0);
        }
        let latency = compander.latency_samples();
        let gain = level_db(&output, latency + frames / 2) - level_db(&input, frames / 2);
        assert!(gain.abs() < 0.05, "transparency gain {gain:+.3} dB");
    }

    #[test]
    fn mix_zero_is_the_delayed_dry_signal() {
        let mut compander = Compander::new(SR);
        compander.set_param("mix", 0.0);
        compander.set_param("limiter", 1.0);
        let latency = compander.latency_samples();
        let frames = 500;
        let mut input = vec![0.0; frames];
        input[100] = 0.5;
        let output = render(&mut compander, &input);
        assert!((output[100 + latency] - 0.5).abs() < 1e-12);
        assert!(
            output[..100 + latency]
                .iter()
                .all(|value| value.abs() < 1e-12)
        );
    }

    #[test]
    fn latency_follows_the_sample_rate_and_the_limiter_switch() {
        for (rate, expected) in [(44_100, 88), (48_000, 96), (96_000, 192)] {
            let compander = Compander::new(rate);
            assert_eq!(compander.latency_samples(), expected);
        }
        let mut compander = Compander::new(SR);
        compander.set_param("limiter", 0.0);
        assert_eq!(compander.latency_samples(), 0);
        compander.set_param("limiter", 1.0);
        assert_eq!(compander.latency_samples(), 96);
    }

    #[test]
    fn band_gain_is_a_static_trim() {
        let mut compander = quiet_compander();
        compander.set_param("band2_gain", 6.0);
        let frames = 48_000;
        let input = sine(700.0, 0.2, frames);
        let output = render(&mut compander, &input);
        let gain = level_db(&output, frames / 2) - level_db(&input, frames / 2);
        // The other bands also contribute to the crossover sum, so a +6 dB mid
        // trim reads a little under +6 dB on the summed output.
        assert!((gain - 6.0).abs() < 0.6, "band gain {gain:+.3} dB");
    }

    #[test]
    fn threshold_clamps_keep_the_expansion_below_compression() {
        let mut compander = Compander::new(SR);
        compander.set_param("threshold", -12.0);
        compander.set_param("threshold_below", 0.0);
        for band in &compander.bands {
            assert!(band.threshold_below_db <= band.threshold_db);
        }
        compander.set_param("threshold", -40.0);
        for band in &compander.bands {
            assert!(band.threshold_below_db <= band.threshold_db);
        }
    }
}
