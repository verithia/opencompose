//! Breath split: a linear-phase complementary split plus a periodicity detector.
//! Defaults bypass bit-exactly; an engaged split reports 128 samples of delay.

use super::AudioEffect;
use super::detector::db_to_linear;

/// Linear-phase high/low pair group delay, in samples. Fixed at every rate.
const SPLIT_DELAY: usize = 128;
const TAPS: usize = 2 * SPLIT_DELAY + 1;
const MIN_SPLIT_HZ: f64 = 1_500.0;
const MAX_SPLIT_HZ: f64 = 6_000.0;
const DEFAULT_SPLIT_HZ: f64 = 2_000.0;
const MIN_BREATH_DB: f64 = -60.0;
const MAX_BREATH_DB: f64 = 12.0;
const MIN_VOICE_DB: f64 = -60.0;
const MAX_VOICE_DB: f64 = 12.0;
const DEFAULT_THRESHOLD: f64 = 0.5;
const MIN_SMOOTHING_MS: f64 = 1.0;
const MAX_SMOOTHING_MS: f64 = 200.0;
const DEFAULT_SMOOTHING_MS: f64 = 20.0;
const DETECT_RATE_HZ: f64 = 12_000.0;
const WINDOW_S: f64 = 0.030;
const HOP_S: f64 = 0.010;
const MIN_PITCH_HZ: f64 = 70.0;
const MAX_PITCH_HZ: f64 = 1_000.0;
const ENERGY_TAU_S: f64 = 0.030;
const SILENCE: f64 = 1e-30;

/// Whether a parameter id names a frequency.
pub fn frequency_param(name: &str) -> bool {
    name == "split_frequency"
}

/// Whether a parameter id names a dB value.
pub fn db_param(name: &str) -> bool {
    matches!(name, "breath_level" | "voice_level")
}

/// Whether a parameter id names a time in milliseconds.
pub fn time_ms_param(name: &str) -> bool {
    name == "smoothing"
}

/// Whether a parameter id names a boolean switch.
pub fn switch_param(name: &str) -> bool {
    name == "monitor"
}

fn design_lowpass(cutoff_hz: f64, sample_rate: f64, taps: &mut [f64; TAPS]) {
    let fc = (cutoff_hz / sample_rate).clamp(0.001, 0.45);
    let mut sum = 0.0;
    for (index, tap) in taps.iter_mut().enumerate() {
        let offset = index as f64 - SPLIT_DELAY as f64;
        let sinc = if offset.abs() < 1e-12 {
            2.0 * fc
        } else {
            (std::f64::consts::TAU * fc * offset).sin() / (std::f64::consts::PI * offset)
        };
        let phase = std::f64::consts::TAU * index as f64 / (TAPS - 1) as f64;
        let window = 0.35875 - 0.48829 * phase.cos() + 0.14128 * (2.0 * phase).cos()
            - 0.01168 * (3.0 * phase).cos();
        *tap = sinc * window;
        sum += *tap;
    }
    let scale = if sum.abs() < 1e-12 { 0.0 } else { 1.0 / sum };
    for tap in taps.iter_mut() {
        *tap *= scale;
    }
}

/// A breath/voice splitter that detects and controls breath energy.
pub struct BreathSplit {
    breath_level_db: f64,
    voice_level_db: f64,
    threshold: f64,
    smoothing_ms: f64,
    monitor: bool,
    split_hz: f64,
    sample_rate: u32,
    taps: Vec<f64>,
    ring_l: Vec<f64>,
    ring_r: Vec<f64>,
    ring_pos: usize,
    decim: usize,
    decim_phase: usize,
    control_frames: usize,
    controls_left: usize,
    detect_ring: Vec<f64>,
    detect_pos: usize,
    detect_scratch: Vec<f64>,
    detect_prefix: Vec<f64>,
    window: usize,
    min_lag: usize,
    max_lag: usize,
    energy_coeff: f64,
    low_energy: f64,
    high_energy: f64,
    smooth_coeff: f64,
    breath_gain: f64,
    target_gain: f64,
    last_confidence: f64,
}

impl BreathSplit {
    /// Prepare the split filters and detector rings at the sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let mut split = Self {
            breath_level_db: 0.0,
            voice_level_db: 0.0,
            threshold: DEFAULT_THRESHOLD,
            smoothing_ms: DEFAULT_SMOOTHING_MS,
            monitor: false,
            split_hz: DEFAULT_SPLIT_HZ
                .min(sample_rate as f64 * 0.45)
                .max(MIN_SPLIT_HZ),
            sample_rate,
            taps: vec![0.0; TAPS],
            ring_l: vec![0.0; 2 * TAPS],
            ring_r: vec![0.0; 2 * TAPS],
            ring_pos: 0,
            decim: 1,
            decim_phase: 0,
            control_frames: 1,
            controls_left: 1,
            detect_ring: Vec::new(),
            detect_pos: 0,
            detect_scratch: Vec::new(),
            detect_prefix: Vec::new(),
            window: 1,
            min_lag: 1,
            max_lag: 1,
            energy_coeff: 0.0,
            low_energy: 0.0,
            high_energy: 0.0,
            smooth_coeff: 0.0,
            breath_gain: 1.0,
            target_gain: 1.0,
            last_confidence: 0.0,
        };
        split.rebuild_detector();
        split.rebuild_taps();
        split.update_smoothing();
        split
    }

    fn bypassed(&self) -> bool {
        !self.monitor && self.breath_level_db == 0.0 && self.voice_level_db == 0.0
    }

    fn rebuild_detector(&mut self) {
        let rate = self.sample_rate as f64;
        self.decim = ((rate / DETECT_RATE_HZ).round() as usize).max(1);
        let decimated = rate / self.decim as f64;
        self.window = ((WINDOW_S * decimated).round() as usize).max(8);
        self.min_lag = ((decimated / MAX_PITCH_HZ).floor() as usize).max(1);
        self.max_lag = ((decimated / MIN_PITCH_HZ).ceil() as usize)
            .min(self.window.saturating_sub(2))
            .max(self.min_lag);
        self.control_frames = ((HOP_S * rate).round() as usize).max(1);
        self.controls_left = self.control_frames;
        self.detect_ring = vec![0.0; self.window + 2];
        self.detect_scratch = vec![0.0; self.window];
        self.detect_prefix = vec![0.0; self.window + 1];
        self.detect_pos = 0;
        self.decim_phase = 0;
        self.energy_coeff = 1.0 - (-1.0 / (ENERGY_TAU_S * rate)).exp();
        self.low_energy = 0.0;
        self.high_energy = 0.0;
    }

    fn rebuild_taps(&mut self) {
        let mut taps = [0.0f64; TAPS];
        design_lowpass(self.split_hz, self.sample_rate as f64, &mut taps);
        self.taps.copy_from_slice(&taps);
        self.clear_state();
    }

    fn update_smoothing(&mut self) {
        let seconds = (self.smoothing_ms * 0.001).max(1e-4);
        self.smooth_coeff = 1.0 - (-1.0 / (seconds * self.sample_rate as f64)).exp();
    }

    fn clear_state(&mut self) {
        self.ring_l.fill(0.0);
        self.ring_r.fill(0.0);
        self.ring_pos = 0;
        self.detect_ring.fill(0.0);
        self.detect_pos = 0;
        self.decim_phase = 0;
        self.detect_scratch.fill(0.0);
        self.detect_prefix.fill(0.0);
        self.low_energy = 0.0;
        self.high_energy = 0.0;
        self.breath_gain = 1.0;
        self.target_gain = 1.0;
        self.last_confidence = 0.0;
        self.controls_left = self.control_frames;
    }

    fn control_tick(&mut self) {
        let len = self.detect_ring.len();
        let start = (self.detect_pos + len - self.window) % len;
        for index in 0..self.window {
            self.detect_scratch[index] = self.detect_ring[(start + index) % len];
        }
        self.detect_prefix[0] = 0.0;
        for index in 0..self.window {
            let sample = self.detect_scratch[index];
            self.detect_prefix[index + 1] = self.detect_prefix[index] + sample * sample;
        }
        let mut peak = 0.0f64;
        for lag in self.min_lag..=self.max_lag {
            let mut correlation = 0.0;
            for index in lag..self.window {
                correlation += self.detect_scratch[index] * self.detect_scratch[index - lag];
            }
            let energy_now = self.detect_prefix[self.window] - self.detect_prefix[lag];
            let energy_lag = self.detect_prefix[self.window - lag];
            let denominator = (energy_now * energy_lag).max(0.0).sqrt();
            if denominator > SILENCE {
                peak = peak.max((correlation / denominator).clamp(0.0, 1.0));
            }
        }
        let low_share = self.low_energy / (self.high_energy + self.low_energy + SILENCE);
        let high_share = 1.0 - low_share;
        // A pitch peak counts only where the low band carries the energy.
        let confidence = (1.0 - peak * low_share) * high_share;
        self.last_confidence = confidence;
        self.target_gain = if confidence > self.threshold {
            db_to_linear(self.breath_level_db)
        } else {
            1.0
        };
    }
}

impl AudioEffect for BreathSplit {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        if self.bypassed() {
            return;
        }
        let voice_gain = db_to_linear(self.voice_level_db);
        let frames = buffer.len() / 2;
        for frame in 0..frames {
            let input_l = buffer[frame * 2] as f64;
            let input_r = buffer[frame * 2 + 1] as f64;
            let position = self.ring_pos;
            self.ring_l[position] = input_l;
            self.ring_l[position + TAPS] = input_l;
            self.ring_r[position] = input_r;
            self.ring_r[position + TAPS] = input_r;
            let start = position + 1;
            let window_l = &self.ring_l[start..start + TAPS];
            let window_r = &self.ring_r[start..start + TAPS];
            let mut low_l = 0.0;
            let mut low_r = 0.0;
            for (tap, (left, right)) in self.taps.iter().zip(window_l.iter().zip(window_r)) {
                low_l += *tap * *left;
                low_r += *tap * *right;
            }
            let delayed_l = self.ring_l[start + SPLIT_DELAY];
            let delayed_r = self.ring_r[start + SPLIT_DELAY];
            let breath_l = delayed_l - low_l;
            let breath_r = delayed_r - low_r;

            let mid_low = 0.5 * (low_l + low_r);
            let mid_breath = 0.5 * (breath_l + breath_r);
            self.low_energy += (mid_low * mid_low - self.low_energy) * self.energy_coeff;
            self.high_energy += (mid_breath * mid_breath - self.high_energy) * self.energy_coeff;

            if self.decim_phase == 0 {
                self.detect_ring[self.detect_pos] = mid_low;
                self.detect_pos = (self.detect_pos + 1) % self.detect_ring.len();
            }
            self.decim_phase += 1;
            if self.decim_phase >= self.decim {
                self.decim_phase = 0;
            }

            self.controls_left -= 1;
            if self.controls_left == 0 {
                self.control_tick();
                self.controls_left = self.control_frames;
            }
            self.breath_gain += (self.target_gain - self.breath_gain) * self.smooth_coeff;
            let gain = self.breath_gain;

            let (output_l, output_r) = if self.monitor {
                (breath_l * gain, breath_r * gain)
            } else {
                (
                    voice_gain * delayed_l + (gain - voice_gain) * breath_l,
                    voice_gain * delayed_r + (gain - voice_gain) * breath_r,
                )
            };
            buffer[frame * 2] = output_l as f32;
            buffer[frame * 2 + 1] = output_r as f32;
            self.ring_pos = (position + 1) % TAPS;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "breath_level" => {
                let bypassed = self.bypassed();
                self.breath_level_db = value.clamp(MIN_BREATH_DB, MAX_BREATH_DB);
                if bypassed != self.bypassed() {
                    self.clear_state();
                }
            }
            "voice_level" => {
                let bypassed = self.bypassed();
                self.voice_level_db = value.clamp(MIN_VOICE_DB, MAX_VOICE_DB);
                if bypassed != self.bypassed() {
                    self.clear_state();
                }
            }
            "threshold" => self.threshold = value.clamp(0.0, 1.0),
            "smoothing" => {
                self.smoothing_ms = value.clamp(MIN_SMOOTHING_MS, MAX_SMOOTHING_MS);
                self.update_smoothing();
            }
            "monitor" => {
                let bypassed = self.bypassed();
                self.monitor = value >= 0.5;
                if bypassed != self.bypassed() {
                    self.clear_state();
                }
            }
            "split_frequency" => {
                self.split_hz = value
                    .clamp(MIN_SPLIT_HZ, MAX_SPLIT_HZ)
                    .min(self.sample_rate as f64 * 0.45);
                self.rebuild_taps();
            }
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "breath_level" => Some(self.breath_level_db),
            "voice_level" => Some(self.voice_level_db),
            "threshold" => Some(self.threshold),
            "smoothing" => Some(self.smoothing_ms),
            "monitor" => Some(if self.monitor { 1.0 } else { 0.0 }),
            "split_frequency" => Some(self.split_hz),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.clear_state();
    }

    fn latency_samples(&self) -> u32 {
        if self.bypassed() {
            0
        } else {
            SPLIT_DELAY as u32
        }
    }

    fn tail_samples(&self) -> u32 {
        if self.bypassed() {
            0
        } else {
            (2 * SPLIT_DELAY) as u32
        }
    }

    fn gain_reduction_db(&self) -> f64 {
        if self.bypassed() {
            0.0
        } else {
            (-20.0 * self.breath_gain.max(SILENCE).log10()).max(0.0)
        }
    }

    fn name(&self) -> &str {
        "breath-split"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biquad::{BiquadFilter, FilterType};

    const SR: u32 = 48_000;

    fn noise(frames: usize, seed: u32) -> Vec<f64> {
        let mut state = seed;
        (0..frames)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f64 / 8_388_608.0 - 1.0
            })
            .collect()
    }

    fn band_noise(frames: usize, low_hz: f64, high_hz: f64, rms: f64, seed: u32) -> Vec<f64> {
        let mut high = [
            BiquadFilter::new(FilterType::HighPass, low_hz, 0.707, 0.0, SR),
            BiquadFilter::new(FilterType::HighPass, low_hz, 0.707, 0.0, SR),
            BiquadFilter::new(FilterType::HighPass, low_hz, 0.707, 0.0, SR),
        ];
        let mut low = [
            BiquadFilter::new(FilterType::LowPass, high_hz, 0.707, 0.0, SR),
            BiquadFilter::new(FilterType::LowPass, high_hz, 0.707, 0.0, SR),
            BiquadFilter::new(FilterType::LowPass, high_hz, 0.707, 0.0, SR),
        ];
        let mut samples: Vec<f64> = noise(frames, seed)
            .into_iter()
            .map(|sample| {
                let mut sample = sample;
                for filter in &mut high {
                    sample = filter.process_sample(0, sample);
                }
                for filter in &mut low {
                    sample = filter.process_sample(0, sample);
                }
                sample
            })
            .collect();
        let power = samples.iter().map(|sample| sample * sample).sum::<f64>() / frames as f64;
        let scale = if power > 0.0 { rms / power.sqrt() } else { 0.0 };
        for sample in &mut samples {
            *sample *= scale;
        }
        samples
    }

    fn vowel(frames: usize, fundamental: f64, amplitude: f64, seed: u32) -> Vec<f64> {
        let mut phase = seed as f64 * 0.017;
        let mut samples = vec![0.0; frames];
        for harmonic in 1..=24 {
            let weight = 1.0 / harmonic as f64;
            let phase_step = std::f64::consts::TAU * fundamental * harmonic as f64 / SR as f64;
            for (index, sample) in samples.iter_mut().enumerate() {
                *sample += weight * (phase_step * index as f64 + phase).sin();
            }
            phase += 0.37;
        }
        let peak = samples
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
        let scale = if peak > 0.0 { amplitude / peak } else { 0.0 };
        for sample in &mut samples {
            *sample *= scale;
        }
        samples
    }

    fn sine(frames: usize, frequency: f64, amplitude: f64) -> Vec<f64> {
        (0..frames)
            .map(|frame| {
                amplitude * (std::f64::consts::TAU * frequency * frame as f64 / SR as f64).sin()
            })
            .collect()
    }

    fn stereo(mono: &[f64]) -> Vec<f32> {
        mono.iter()
            .flat_map(|sample| [*sample as f32, *sample as f32])
            .collect()
    }

    fn run(effect: &mut BreathSplit, input: &[f32], block_frames: usize) -> Vec<f32> {
        let mut output = input.to_vec();
        for chunk in output.chunks_mut(block_frames * 2) {
            effect.process(chunk, SR);
        }
        output
    }

    fn rms(samples: &[f32], start: usize, end: usize) -> f64 {
        let mut power = 0.0;
        for frame in start..end {
            power += (samples[frame * 2] as f64).powi(2);
        }
        (power / (end - start).max(1) as f64).sqrt()
    }

    fn max_difference(output: &[f32], input: &[f32], delay: usize) -> f64 {
        let frames = output.len() / 2;
        let mut maximum = 0.0f64;
        for frame in delay..frames {
            maximum = maximum.max((output[frame * 2] - input[(frame - delay) * 2]).abs() as f64);
        }
        maximum
    }

    fn max_step(samples: &[f32], delay: usize) -> f64 {
        let frames = samples.len() / 2;
        let mut maximum = 0.0f64;
        for frame in (delay + 1)..frames {
            maximum = maximum.max((samples[frame * 2] - samples[(frame - 1) * 2]).abs() as f64);
        }
        maximum
    }

    #[test]
    fn defaults_are_a_bit_exact_no_op() {
        let mixed: Vec<f32> = (0..SR as usize)
            .flat_map(|frame| {
                let time = frame as f64 / SR as f64;
                let value = 0.2 * (std::f64::consts::TAU * 220.0 * time).sin()
                    + 0.05 * (std::f64::consts::TAU * 5_000.0 * time).sin();
                [value as f32, value as f32]
            })
            .collect();
        let mut effect = BreathSplit::new(SR);
        assert_eq!(effect.latency_samples(), 0);
        let mut output = mixed.clone();
        effect.process(&mut output, SR);
        assert_eq!(output, mixed);

        for (name, value) in [
            ("breath_level", 0.0),
            ("voice_level", 0.0),
            ("threshold", DEFAULT_THRESHOLD),
            ("smoothing", DEFAULT_SMOOTHING_MS),
            ("monitor", 0.0),
            ("split_frequency", DEFAULT_SPLIT_HZ),
        ] {
            effect.set_param(name, value);
        }
        let mut output = mixed.clone();
        effect.process(&mut output, SR);
        assert_eq!(output, mixed);
        assert_eq!(effect.latency_samples(), 0);
    }

    #[test]
    fn voice_only_material_is_bit_exact_at_unity() {
        let input = stereo(&vowel(2 * SR as usize, 150.0, 0.3, 1));
        let mut effect = BreathSplit::new(SR);
        effect.set_param("breath_level", -24.0);
        effect.set_param("threshold", 0.5);
        assert_eq!(effect.latency_samples(), SPLIT_DELAY as u32);
        let output = run(&mut effect, &input, 480);
        assert!(
            max_difference(&output, &input, SPLIT_DELAY) < 1e-6,
            "voice path moved by {}",
            max_difference(&output, &input, SPLIT_DELAY)
        );
    }

    #[test]
    fn synthetic_breath_is_attenuated_by_the_requested_amount() {
        let input = stereo(&band_noise(2 * SR as usize, 4_000.0, 8_000.0, 0.1, 7));
        for requested in [-12.0, -24.0, 6.0] {
            let mut effect = BreathSplit::new(SR);
            effect.set_param("breath_level", requested);
            effect.set_param("threshold", 0.5);
            effect.set_param("smoothing", 20.0);
            let output = run(&mut effect, &input, 480);
            let start = SPLIT_DELAY + SR as usize / 2;
            let gain = 20.0
                * (rms(&output, start, output.len() / 2)
                    / rms(&input, SR as usize / 2, SR as usize))
                .log10();
            assert!(
                (gain - requested).abs() < 0.05,
                "requested {requested} dB, measured {gain} dB"
            );
        }
    }

    #[test]
    fn synthetic_vowel_is_untouched_at_any_breath_level() {
        let input = stereo(&vowel(2 * SR as usize, 150.0, 0.3, 3));
        for requested in [-12.0, -40.0] {
            let mut effect = BreathSplit::new(SR);
            effect.set_param("breath_level", requested);
            effect.set_param("smoothing", 20.0);
            let output = run(&mut effect, &input, 480);
            let movement = max_difference(&output, &input, SPLIT_DELAY);
            assert!(
                movement < 1e-6,
                "{requested} dB moved the vowel by {movement}"
            );
            let gain = 20.0
                * (rms(&output, SPLIT_DELAY + SR as usize / 2, output.len() / 2)
                    / rms(&input, SR as usize / 2, SR as usize))
                .log10();
            assert!((gain).abs() < 0.3, "vowel gain {gain} dB");
        }
    }

    #[test]
    fn split_sums_to_the_input_within_1e_6_at_unity() {
        let mut mono = vowel(2 * SR as usize, 150.0, 0.2, 5);
        let breath = band_noise(2 * SR as usize, 4_000.0, 8_000.0, 0.05, 9);
        for (sample, noise) in mono.iter_mut().zip(breath) {
            *sample += noise;
        }
        let input = stereo(&mono);
        let mut effect = BreathSplit::new(SR);
        effect.set_param("breath_level", -60.0);
        effect.set_param("threshold", 1.0);
        let output = run(&mut effect, &input, 512);
        let error = max_difference(&output, &input, SPLIT_DELAY);
        assert!(error < 1e-6, "unity split error {error}");
    }

    #[test]
    fn mixed_phrase_transition_adds_no_step_above_minus_sixty_dbfs() {
        let voiced = vowel(3 * SR as usize / 4, 150.0, 0.08, 11);
        let breath = band_noise(3 * SR as usize / 4, 4_000.0, 8_000.0, 0.08, 13);
        let mut mono = Vec::with_capacity(2 * SR as usize);
        mono.extend_from_slice(&voiced);
        mono.extend_from_slice(&breath);
        mono.extend_from_slice(&voiced);
        let input = stereo(&mono);
        let mut effect = BreathSplit::new(SR);
        effect.set_param("breath_level", -30.0);
        effect.set_param("smoothing", 20.0);
        let output = run(&mut effect, &input, 480);
        let input_step = max_step(&input, 0);
        let output_step = max_step(&output, SPLIT_DELAY);
        assert!(
            output_step <= input_step + 1e-3,
            "transition added a {:.6} step over the source's {:.6}",
            output_step - input_step,
            input_step
        );
    }

    #[test]
    fn mixed_phrase_transition_stays_below_minus_sixty_dbfs() {
        let voiced = sine(3 * SR as usize / 4, 80.0, 0.03);
        let breath = band_noise(3 * SR as usize / 4, 4_000.0, 8_000.0, 0.00005, 53);
        let mut mono = voiced;
        mono.extend(breath);
        mono.extend(sine(3 * SR as usize / 4, 80.0, 0.03));
        let input = stereo(&mono);
        let mut effect = BreathSplit::new(SR);
        effect.set_param("breath_level", -30.0);
        effect.set_param("smoothing", 20.0);
        let output = run(&mut effect, &input, 480);
        assert!(max_step(&input, 0) < 1e-3);
        assert!(
            max_step(&output, SPLIT_DELAY) < 1e-3,
            "transition step {}",
            max_step(&output, SPLIT_DELAY)
        );
    }

    #[test]
    fn detector_window_is_thirty_ms_with_a_ten_ms_hop() {
        let effect = BreathSplit::new(SR);
        assert_eq!(effect.window, 360);
        assert_eq!(effect.control_frames, 480);
        assert_eq!(effect.min_lag, 12);
        assert_eq!(effect.max_lag, 172);
        assert_eq!(effect.decim, 4);
    }

    #[test]
    fn smoothing_limits_the_gain_motion_per_sample() {
        let mut mono = vowel(SR as usize / 4, 150.0, 0.08, 17);
        mono.extend(band_noise(SR as usize / 4, 4_000.0, 8_000.0, 0.08, 19));
        mono.extend(vowel(SR as usize / 4, 150.0, 0.08, 23));
        let input = stereo(&mono);
        let mut effect = BreathSplit::new(SR);
        effect.set_param("breath_level", -30.0);
        effect.set_param("smoothing", 20.0);
        let mut previous = effect.breath_gain;
        let mut maximum = 0.0f64;
        let mut output = input.clone();
        for chunk in output.chunks_mut(2) {
            effect.process(chunk, SR);
            maximum = maximum.max((effect.breath_gain - previous).abs());
            previous = effect.breath_gain;
        }
        assert!(maximum < 2e-3, "gain stepped by {maximum} per sample");
    }

    #[test]
    fn threshold_gates_the_breath_path() {
        let input = stereo(&band_noise(SR as usize, 4_000.0, 8_000.0, 0.1, 29));
        let mut gated = BreathSplit::new(SR);
        gated.set_param("breath_level", -12.0);
        gated.set_param("threshold", 0.0);
        let output = run(&mut gated, &input, 480);
        let start = SPLIT_DELAY + SR as usize / 2;
        let gain = 20.0
            * (rms(&output, start, output.len() / 2) / rms(&input, SR as usize / 2, SR as usize))
                .log10();
        assert!((gain + 12.0).abs() < 0.3, "{gain} dB");

        let mut open = BreathSplit::new(SR);
        open.set_param("breath_level", -12.0);
        open.set_param("threshold", 1.0);
        let output = run(&mut open, &input, 480);
        assert!(max_difference(&output, &input, SPLIT_DELAY) < 1e-6);
    }

    #[test]
    fn monitor_plays_only_the_breath_path() {
        let low = stereo(
            &(0..SR as usize)
                .map(|frame| {
                    0.25 * (std::f64::consts::TAU * 1_000.0 * frame as f64 / SR as f64).sin()
                })
                .collect::<Vec<_>>(),
        );
        let high = stereo(
            &(0..SR as usize)
                .map(|frame| {
                    0.25 * (std::f64::consts::TAU * 6_000.0 * frame as f64 / SR as f64).sin()
                })
                .collect::<Vec<_>>(),
        );
        let mut monitor = BreathSplit::new(SR);
        monitor.set_param("monitor", 1.0);
        monitor.set_param("threshold", 1.0);
        let low_out = run(&mut monitor, &low, 480);
        let high_out = run(&mut monitor, &high, 480);
        let start = SPLIT_DELAY + SR as usize / 2;
        let low_gain = 20.0
            * (rms(&low_out, start, low_out.len() / 2) / rms(&low, SR as usize / 2, SR as usize))
                .log10();
        let high_gain = 20.0
            * (rms(&high_out, start, high_out.len() / 2)
                / rms(&high, SR as usize / 2, SR as usize))
            .log10();
        assert!(
            low_gain < -20.0,
            "1 kHz leaked into the monitor: {low_gain} dB"
        );
        assert!(high_gain.abs() < 0.5, "6 kHz monitor gain {high_gain} dB");
    }

    #[test]
    fn latency_is_reported_and_the_impulse_lands_there() {
        let mut bypass = BreathSplit::new(SR);
        assert_eq!(bypass.latency_samples(), 0);
        assert_eq!(bypass.tail_samples(), 0);
        bypass.set_param("breath_level", -12.0);
        assert_eq!(bypass.latency_samples(), SPLIT_DELAY as u32);
        assert_eq!(bypass.tail_samples(), (2 * SPLIT_DELAY) as u32);

        let mut input = vec![0.0f32; 8_000 * 2];
        input[2_000] = 0.5;
        input[2_001] = 0.5;
        let mut effect = BreathSplit::new(SR);
        effect.set_param("breath_level", -12.0);
        effect.set_param("threshold", 1.0);
        effect.process(&mut input, SR);
        let onset = input
            .chunks_exact(2)
            .position(|frame| frame[0].abs() > 1e-9)
            .expect("delayed impulse");
        assert_eq!(onset, 1_000 + SPLIT_DELAY);
        assert_eq!(input[onset * 2], 0.5);
    }

    #[test]
    fn reset_is_deterministic() {
        let input = stereo(&band_noise(2 * SR as usize, 4_000.0, 8_000.0, 0.1, 31));
        let mut effect = BreathSplit::new(SR);
        effect.set_param("breath_level", -18.0);
        let first = run(&mut effect, &input, 480);
        effect.reset();
        let second = run(&mut effect, &input, 480);
        assert_eq!(first, second);
    }

    #[test]
    #[ignore]
    fn measurements_for_docs() {
        let seconds = 2 * SR as usize;
        let mixed = stereo(&band_noise(seconds, 4_000.0, 8_000.0, 0.1, 7));
        for requested in [-12.0, -24.0, 6.0] {
            let mut effect = BreathSplit::new(SR);
            effect.set_param("breath_level", requested);
            effect.set_param("threshold", 0.5);
            effect.set_param("smoothing", 20.0);
            let output = run(&mut effect, &mixed, 480);
            let gain = 20.0
                * (rms(
                    &output,
                    SPLIT_DELAY + SR as usize / 2,
                    SPLIT_DELAY + SR as usize,
                ) / rms(&mixed, SR as usize / 2, SR as usize))
                .log10();
            println!("attenuation requested {requested:+.1} dB -> {gain:+.3} dB");
        }

        for (low, high) in [(2_000.0, 4_000.0), (4_000.0, 8_000.0), (6_000.0, 10_000.0)] {
            let band = stereo(&band_noise(seconds, low, high, 0.1, 7));
            let mut effect = BreathSplit::new(SR);
            effect.set_param("breath_level", -12.0);
            effect.set_param("threshold", 0.5);
            let output = run(&mut effect, &band, 480);
            let gain = 20.0
                * (rms(
                    &output,
                    SPLIT_DELAY + SR as usize / 2,
                    SPLIT_DELAY + SR as usize,
                ) / rms(&band, SR as usize / 2, SR as usize))
                .log10();
            println!("band {low:.0}-{high:.0} Hz -> {gain:+.3} dB");
        }

        let vowel_input = stereo(&vowel(seconds, 150.0, 0.3, 3));
        for requested in [-12.0, -30.0] {
            let mut effect = BreathSplit::new(SR);
            effect.set_param("breath_level", requested);
            effect.set_param("smoothing", 20.0);
            let output = run(&mut effect, &vowel_input, 480);
            let movement = max_difference(&output, &vowel_input, SPLIT_DELAY);
            let gain = 20.0
                * (rms(
                    &output,
                    SPLIT_DELAY + SR as usize / 2,
                    SPLIT_DELAY + SR as usize,
                ) / rms(&vowel_input, SR as usize / 2, SR as usize))
                .log10();
            println!("vowel at {requested:+.0} dB -> movement {movement:.3e}, RMS {gain:+.3} dB");
        }

        let mut purity = vowel(3 * SR as usize / 4, 150.0, 0.08, 11);
        let breath = band_noise(3 * SR as usize / 4, 4_000.0, 8_000.0, 0.08, 13);
        purity.extend_from_slice(&breath);
        purity.extend(vowel(3 * SR as usize / 4, 150.0, 0.08, 11));
        let phrase = stereo(&purity);
        let mut effect = BreathSplit::new(SR);
        effect.set_param("breath_level", -30.0);
        effect.set_param("smoothing", 20.0);
        let output = run(&mut effect, &phrase, 480);
        let input_step = max_step(&phrase, 0);
        let output_step = max_step(&output, SPLIT_DELAY);
        println!(
            "click: source max step {input_step:.6}, output {output_step:.6}, added {:.6}",
            output_step - input_step
        );

        let mut low_phrase = sine(3 * SR as usize / 4, 80.0, 0.03);
        low_phrase.extend(band_noise(
            3 * SR as usize / 4,
            4_000.0,
            8_000.0,
            0.00005,
            53,
        ));
        low_phrase.extend(sine(3 * SR as usize / 4, 80.0, 0.03));
        let low_phrase = stereo(&low_phrase);
        let mut low_effect = BreathSplit::new(SR);
        low_effect.set_param("breath_level", -30.0);
        let low_output = run(&mut low_effect, &low_phrase, 480);
        println!(
            "low-level transition: source {:.6}, output {:.6}",
            max_step(&low_phrase, 0),
            max_step(&low_output, SPLIT_DELAY)
        );

        let mut effect = BreathSplit::new(SR);
        effect.set_param("breath_level", -12.0);
        effect.set_param("threshold", 1.0);
        let reference = run(&mut effect, &phrase, 480);
        println!(
            "unity split max error {:.3e}",
            max_difference(&reference, &phrase, SPLIT_DELAY)
        );
        println!(
            "latency default {} / engaged {}, tail {}",
            BreathSplit::new(SR).latency_samples(),
            effect.latency_samples(),
            effect.tail_samples()
        );

        for (label, input) in [
            (
                "breath",
                stereo(&band_noise(seconds, 4_000.0, 8_000.0, 0.1, 41)),
            ),
            ("vowel", stereo(&vowel(seconds, 150.0, 0.3, 43))),
        ] {
            let mut effect = BreathSplit::new(SR);
            effect.set_param("breath_level", -12.0);
            let mut output = input.clone();
            let mut min = 1.0f64;
            let mut max = 0.0f64;
            for chunk in output.chunks_mut(480 * 2) {
                effect.process(chunk, SR);
                min = min.min(effect.last_confidence);
                max = max.max(effect.last_confidence);
            }
            println!("{label} confidence {min:.3}..{max:.3}");
        }
    }

    #[test]
    fn extreme_settings_stay_finite() {
        let mut mono = band_noise(SR as usize, 4_000.0, 8_000.0, 0.2, 37);
        mono[1_000] = 0.9;
        let input = stereo(&mono);
        for (breath, voice, threshold, smoothing, split, monitor) in [
            (-60.0, -60.0, 0.0, 1.0, 1_500.0, 0.0),
            (12.0, 12.0, 0.0, 200.0, 6_000.0, 0.0),
            (12.0, -60.0, 1.0, 200.0, 6_000.0, 1.0),
            (-60.0, 12.0, 1.0, 1.0, 1_500.0, 1.0),
        ] {
            let mut effect = BreathSplit::new(SR);
            effect.set_param("breath_level", breath);
            effect.set_param("voice_level", voice);
            effect.set_param("threshold", threshold);
            effect.set_param("smoothing", smoothing);
            effect.set_param("split_frequency", split);
            effect.set_param("monitor", monitor);
            let output = run(&mut effect, &input, 480);
            assert!(output.iter().all(|sample| sample.is_finite()));
            assert!(
                output.iter().all(|sample| sample.abs() <= 8.0),
                "unbounded output"
            );
        }
    }
}
