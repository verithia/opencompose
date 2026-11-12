//! Plosive: a low-band burst detector driving a dynamic low shelf.
//!
//! Two power envelopes per band (fast 1/15 ms, slow 40/250 ms) measure the
//! rise, and a burst must carry most of its excess energy below the shelf
//! corner. The action is a time-interpolated low shelf, so unity gain is
//! bit-exact, the reported latency is zero and `amount = 0` never touches the
//! buffer.

use super::AudioEffect;
use super::biquad::{BiquadCoefficients, BiquadFilter, BiquadState, FilterType};

const MIN_FREQUENCY_HZ: f64 = 80.0;
const MAX_FREQUENCY_HZ: f64 = 150.0;
const DEFAULT_FREQUENCY_HZ: f64 = 120.0;
const MIN_RELEASE_MS: f64 = 10.0;
const MAX_RELEASE_MS: f64 = 300.0;
const DEFAULT_RELEASE_MS: f64 = 40.0;
const MIN_THRESHOLD_DB: f64 = 2.0;
const MAX_THRESHOLD_DB: f64 = 24.0;
const DEFAULT_THRESHOLD_DB: f64 = 6.0;
/// Full `amount` removes this much at DC; 50 % removes 6 dB.
const MAX_DEPTH_DB: f64 = 12.0;

const BUTTERWORTH_Q_LOW: f64 = 0.541_196_1;
const BUTTERWORTH_Q_HIGH: f64 = 1.306_563;
/// Fixed eight-decimal low-shelf coefficient for the S = 1 slope. Retain its
/// rounding so changing the numeric representation does not alter filter audio.
const SHELF_Q: f64 = 70_710_678.0 / 100_000_000.0;
/// HF sidechain that sees the burst's broadband noise over a sung note.
const TRANSIENT_HZ: f64 = 400.0;
const FAST_ATTACK_S: f64 = 0.001;
const FAST_RELEASE_S: f64 = 0.015;
const REFERENCE_ATTACK_S: f64 = 0.040;
const REFERENCE_RELEASE_S: f64 = 0.250;
const ENGAGE_ATTACK_S: f64 = 0.0015;
const HOLD_S: f64 = 0.020;
const CONTROL_S: f64 = 0.001;
/// Share of the excess energy that must sit in the low band.
const LOW_EXCESS_SHARE: f64 = 0.5;
/// -80 dBFS power; below it the rise ratio is numerical noise.
const LEVEL_FLOOR: f64 = 1e-8;
const SILENCE: f64 = 1e-30;

/// Whether a parameter id names a frequency.
pub fn frequency_param(name: &str) -> bool {
    name == "frequency"
}

/// Whether a parameter id names a dB value.
pub fn db_param(name: &str) -> bool {
    name == "threshold"
}

/// Whether a parameter id names a time in milliseconds.
pub fn time_ms_param(name: &str) -> bool {
    name == "release"
}

/// Whether a parameter id names a percentage.
pub fn percent_param(name: &str) -> bool {
    name == "amount"
}

/// Whether a parameter id names a boolean switch.
pub fn switch_param(name: &str) -> bool {
    name == "monitor"
}

/// A plosive suppressor that ducks low-frequency bursts below the voice band.
pub struct Plosive {
    amount_percent: f64,
    threshold_db: f64,
    frequency_hz: f64,
    release_ms: f64,
    monitor: bool,
    sample_rate: u32,
    detector_low_a: BiquadFilter,
    detector_low_b: BiquadFilter,
    detector_high: BiquadFilter,
    shelf: BiquadCoefficients,
    shelf_step: BiquadCoefficients,
    action: [BiquadState; 2],
    fast_attack: f64,
    fast_release: f64,
    reference_attack: f64,
    reference_release: f64,
    engage_attack: f64,
    engage_release: f64,
    control_frames: usize,
    controls_left: usize,
    hold_frames: usize,
    hold_left: usize,
    low_fast: f64,
    low_reference: f64,
    transient_fast: f64,
    transient_reference: f64,
    engagement: f64,
    last_rise_db: f64,
    last_share: f64,
    last_low_excess: f64,
    last_transient_excess: f64,
    peak_reduction_db: f64,
}

#[inline]
fn follow(current: f64, target: f64, up: f64, down: f64) -> f64 {
    current + (target - current) * if target > current { up } else { down }
}

#[inline]
fn one_pole(seconds: f64, sample_rate: f64) -> f64 {
    1.0 - (-1.0 / (seconds.max(1e-5) * sample_rate)).exp()
}

impl Plosive {
    /// Prepare the detector filters and shelf at the sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let mut device = Self {
            amount_percent: 0.0,
            threshold_db: DEFAULT_THRESHOLD_DB,
            frequency_hz: DEFAULT_FREQUENCY_HZ,
            release_ms: DEFAULT_RELEASE_MS,
            monitor: false,
            sample_rate,
            detector_low_a: BiquadFilter::new(
                FilterType::LowPass,
                DEFAULT_FREQUENCY_HZ,
                BUTTERWORTH_Q_LOW,
                0.0,
                sample_rate,
            ),
            detector_low_b: BiquadFilter::new(
                FilterType::LowPass,
                DEFAULT_FREQUENCY_HZ,
                BUTTERWORTH_Q_HIGH,
                0.0,
                sample_rate,
            ),
            detector_high: BiquadFilter::new(
                FilterType::HighPass,
                TRANSIENT_HZ,
                0.707,
                0.0,
                sample_rate,
            ),
            shelf: BiquadCoefficients::default(),
            shelf_step: BiquadCoefficients::default(),
            action: [BiquadState::default(); 2],
            fast_attack: 0.0,
            fast_release: 0.0,
            reference_attack: 0.0,
            reference_release: 0.0,
            engage_attack: 0.0,
            engage_release: 0.0,
            control_frames: 1,
            controls_left: 1,
            hold_frames: 1,
            hold_left: 0,
            low_fast: 0.0,
            low_reference: 0.0,
            transient_fast: 0.0,
            transient_reference: 0.0,
            engagement: 0.0,
            last_rise_db: 0.0,
            last_share: 0.0,
            last_low_excess: 0.0,
            last_transient_excess: 0.0,
            peak_reduction_db: 0.0,
        };
        device.update_rates();
        device.update_frequency();
        device
    }

    fn bypassed(&self) -> bool {
        self.amount_percent <= 0.0
    }

    fn depth_db(&self) -> f64 {
        MAX_DEPTH_DB * self.amount_percent * 0.01
    }

    fn identity_shelf(&mut self) {
        self.engagement = 0.0;
        self.retarget_now();
    }

    /// Jump the shelf to the current engagement, for a parameter change or a
    /// reset; normal operation glides through `retarget_shelf`.
    fn retarget_now(&mut self) {
        self.shelf = BiquadCoefficients::design(
            FilterType::LowShelf,
            self.frequency_hz,
            SHELF_Q,
            -self.depth_db() * self.engagement,
            self.sample_rate as f64,
        );
        self.shelf_step = BiquadCoefficients::default();
    }

    fn update_frequency(&mut self) {
        self.frequency_hz = self.frequency_hz.clamp(MIN_FREQUENCY_HZ, MAX_FREQUENCY_HZ);
        self.detector_low_a
            .set_params(self.frequency_hz, BUTTERWORTH_Q_LOW, 0.0);
        self.detector_low_b
            .set_params(self.frequency_hz, BUTTERWORTH_Q_HIGH, 0.0);
        self.retarget_now();
    }

    fn update_rates(&mut self) {
        let rate = self.sample_rate as f64;
        self.fast_attack = one_pole(FAST_ATTACK_S, rate);
        self.fast_release = one_pole(FAST_RELEASE_S, rate);
        self.reference_attack = one_pole(REFERENCE_ATTACK_S, rate);
        self.reference_release = one_pole(REFERENCE_RELEASE_S, rate);
        self.engage_attack = one_pole(ENGAGE_ATTACK_S, rate);
        self.engage_release = one_pole(self.release_ms * 0.001, rate);
        self.control_frames = ((CONTROL_S * rate).round() as usize).max(1);
        self.hold_frames = ((HOLD_S * rate).round() as usize).max(1);
        self.controls_left = self.control_frames;
        self.shelf_step = BiquadCoefficients::default();
    }

    fn clear_state(&mut self) {
        self.detector_low_a.reset();
        self.detector_low_b.reset();
        self.detector_high.reset();
        self.action[0].reset();
        self.action[1].reset();
        self.identity_shelf();
        self.low_fast = 0.0;
        self.low_reference = 0.0;
        self.transient_fast = 0.0;
        self.transient_reference = 0.0;
        self.engagement = 0.0;
        self.hold_left = 0;
        self.controls_left = self.control_frames;
        self.last_rise_db = 0.0;
        self.last_share = 0.0;
        self.last_low_excess = 0.0;
        self.last_transient_excess = 0.0;
        self.peak_reduction_db = 0.0;
    }

    /// One control tick: the burst test runs at `CONTROL_S`, never per block.
    fn control_tick(&mut self) {
        let fast = self.low_fast + self.transient_fast;
        let reference = self.low_reference + self.transient_reference;
        self.last_rise_db = 10.0 * ((fast + SILENCE) / (reference + SILENCE)).log10();
        let excess_low = (self.low_fast - self.low_reference).max(0.0);
        let excess_transient = (self.transient_fast - self.transient_reference).max(0.0);
        self.last_low_excess = excess_low;
        self.last_transient_excess = excess_transient;
        self.last_share = excess_low / (excess_low + excess_transient + SILENCE);
        if fast > LEVEL_FLOOR
            && self.last_rise_db >= self.threshold_db
            && self.last_share >= LOW_EXCESS_SHARE
        {
            self.hold_left = self.hold_frames;
        }
    }

    /// Retarget the shelf from the current engagement, interpolated over one
    /// control block so the coefficients never step.
    fn retarget_shelf(&mut self) {
        let target = BiquadCoefficients::design(
            FilterType::LowShelf,
            self.frequency_hz,
            SHELF_Q,
            -self.depth_db() * self.engagement,
            self.sample_rate as f64,
        );
        self.shelf_step = self.shelf.step_toward(&target, self.control_frames as f64);
    }
}

impl AudioEffect for Plosive {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        if self.bypassed() {
            return;
        }
        let frames = buffer.len() / 2;
        for frame in 0..frames {
            let input_l = buffer[frame * 2] as f64;
            let input_r = buffer[frame * 2 + 1] as f64;
            let mid = 0.5 * (input_l + input_r);
            let low = self
                .detector_low_b
                .process_sample(0, self.detector_low_a.process_sample(0, mid));
            let high = self.detector_high.process_sample(0, mid);

            self.low_fast = follow(
                self.low_fast,
                low * low,
                self.fast_attack,
                self.fast_release,
            );
            self.low_reference = follow(
                self.low_reference,
                low * low,
                self.reference_attack,
                self.reference_release,
            );
            self.transient_fast = follow(
                self.transient_fast,
                high * high,
                self.fast_attack,
                self.fast_release,
            );
            self.transient_reference = follow(
                self.transient_reference,
                high * high,
                self.reference_attack,
                self.reference_release,
            );

            self.controls_left -= 1;
            let ticked = self.controls_left == 0;
            if ticked {
                self.control_tick();
                self.controls_left = self.control_frames;
            }
            let engaged = self.hold_left > 0;
            if engaged {
                self.hold_left -= 1;
            }
            let target = if engaged { 1.0 } else { 0.0 };
            self.engagement = follow(
                self.engagement,
                target,
                self.engage_attack,
                self.engage_release,
            );
            if ticked {
                self.retarget_shelf();
            }
            self.shelf.advance(&self.shelf_step);
            let reduction = self.depth_db() * self.engagement;
            self.peak_reduction_db = self.peak_reduction_db.max(reduction);

            let output_l = self.action[0].process(&self.shelf, input_l);
            let output_r = self.action[1].process(&self.shelf, input_r);
            let (write_l, write_r) = if self.monitor {
                (input_l - output_l, input_r - output_r)
            } else {
                (output_l, output_r)
            };
            buffer[frame * 2] = write_l as f32;
            buffer[frame * 2 + 1] = write_r as f32;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "amount" => {
                let was = self.bypassed();
                self.amount_percent = value.clamp(0.0, 100.0);
                if was != self.bypassed() {
                    self.clear_state();
                }
            }
            "threshold" => self.threshold_db = value.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB),
            "frequency" => {
                self.frequency_hz = value.clamp(MIN_FREQUENCY_HZ, MAX_FREQUENCY_HZ);
                self.update_frequency();
            }
            "release" => {
                self.release_ms = value.clamp(MIN_RELEASE_MS, MAX_RELEASE_MS);
                self.update_rates();
            }
            "monitor" => self.monitor = value >= 0.5,
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "amount" => Some(self.amount_percent),
            "threshold" => Some(self.threshold_db),
            "frequency" => Some(self.frequency_hz),
            "release" => Some(self.release_ms),
            "monitor" => Some(if self.monitor { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.clear_state();
    }

    fn latency_samples(&self) -> u32 {
        0
    }

    fn tail_samples(&self) -> u32 {
        if self.bypassed() {
            0
        } else {
            (0.01 * self.sample_rate as f64).round() as u32
        }
    }

    fn gain_reduction_db(&self) -> f64 {
        if self.bypassed() {
            0.0
        } else {
            self.depth_db() * self.engagement
        }
    }

    fn name(&self) -> &str {
        "plosive"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelf_coefficient_preserves_specified_decimal_rounding() {
        assert_eq!(SHELF_Q.to_bits(), 0x3fe6_a09e_65dc_27df);
    }

    use rustfft::FftPlanner;
    use rustfft::num_complex::Complex;

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

    fn low_band_noise(frames: usize, cutoff: f64, rms: f64, seed: u32) -> Vec<f64> {
        let mut a = BiquadFilter::new(FilterType::LowPass, cutoff, BUTTERWORTH_Q_LOW, 0.0, SR);
        let mut b = BiquadFilter::new(FilterType::LowPass, cutoff, BUTTERWORTH_Q_HIGH, 0.0, SR);
        let mut samples: Vec<f64> = noise(frames, seed)
            .into_iter()
            .map(|sample| b.process_sample(0, a.process_sample(0, sample)))
            .collect();
        let power = samples.iter().map(|s| s * s).sum::<f64>() / frames as f64;
        let scale = if power > 0.0 { rms / power.sqrt() } else { 0.0 };
        for sample in &mut samples {
            *sample *= scale;
        }
        samples
    }

    fn formant_gain(freq: f64) -> f64 {
        let mut gain = 1.0;
        for (centre, q) in [(700.0, 8.0), (1_220.0, 10.0), (2_600.0, 12.0)] {
            let ratio = freq / centre;
            let denominator = ((1.0 - ratio * ratio).powi(2) + (ratio / q).powi(2)).sqrt();
            gain *= 1.0 / denominator.max(0.02);
        }
        gain
    }

    fn vowel(frames: usize, fundamental: f64, amplitude: f64) -> Vec<f64> {
        let mut samples = vec![0.0; frames];
        for harmonic in 1..=48 {
            let freq = fundamental * harmonic as f64;
            if freq >= 16_000.0 {
                break;
            }
            let weight = formant_gain(freq) / harmonic as f64;
            let phase = harmonic as f64 * 0.37;
            let step = std::f64::consts::TAU * freq / SR as f64;
            for (index, sample) in samples.iter_mut().enumerate() {
                *sample += weight * (step * index as f64 + phase).sin();
            }
        }
        let peak = samples.iter().fold(0.0_f64, |peak, s| peak.max(s.abs()));
        let scale = if peak > 0.0 { amplitude / peak } else { 0.0 };
        for sample in &mut samples {
            *sample *= scale;
        }
        samples
    }

    /// S8: a 30 ms broadband burst whose energy sits mostly below 120 Hz.
    fn plosive_burst(frames: usize) -> Vec<f64> {
        let low = low_band_noise(frames, 120.0, 0.25, 0x51ce_0001);
        let broadband = noise(frames, 0x51ce_0002);
        let mut out = vec![0.0; frames];
        let rise = 0.002;
        let fall_start = 0.027;
        for index in 0..frames {
            let time = index as f64 / SR as f64;
            let envelope = if time < rise {
                time / rise
            } else if time <= fall_start {
                1.0
            } else {
                ((0.030 - time) / (0.030 - fall_start)).clamp(0.0, 1.0)
            };
            out[index] = (low[index] + 0.03 * broadband[index]) * envelope;
        }
        out
    }

    fn signal(burst: Option<f64>, f0: f64, total_s: f64) -> Vec<f32> {
        let frames = (total_s * SR as f64) as usize;
        let vowel_start = if burst.is_some() {
            (0.05 * SR as f64) as usize + (0.035 * SR as f64) as usize
        } else {
            0
        };
        let mut mono = vec![0.0; frames];
        if let Some(amplitude) = burst {
            let burst_frames = (0.030 * SR as f64) as usize;
            let burst_start = (0.05 * SR as f64) as usize;
            let samples = plosive_burst(burst_frames);
            for (index, sample) in samples.iter().enumerate() {
                mono[burst_start + index] += amplitude * sample;
            }
        }
        let body = vowel(frames - vowel_start, f0, 0.35);
        for (index, sample) in body.iter().enumerate() {
            mono[vowel_start + index] += sample;
        }
        mono.iter()
            .flat_map(|sample| [*sample as f32, *sample as f32])
            .collect()
    }

    fn signal_with_burst_over_note(f0: f64) -> Vec<f32> {
        let mut input = signal(None, f0, 0.45);
        let burst = plosive_burst((0.030 * SR as f64) as usize);
        let start = (0.05 * SR as f64) as usize;
        for (index, sample) in burst.iter().enumerate() {
            let value = input[(start + index) * 2] as f64 + sample;
            input[(start + index) * 2] = value as f32;
            input[(start + index) * 2 + 1] = value as f32;
        }
        input
    }

    fn run(effect: &mut Plosive, input: &[f32], block_frames: usize) -> Vec<f32> {
        let mut output = input.to_vec();
        for chunk in output.chunks_mut(block_frames * 2) {
            effect.process(chunk, SR);
        }
        output
    }

    fn mono(samples: &[f32]) -> Vec<f64> {
        samples.chunks(2).map(|pair| pair[0] as f64).collect()
    }

    fn band_energy(mono: &[f64], start: usize, end: usize, low: f64, high: f64) -> f64 {
        let length = end - start;
        let mut size = 4_096;
        while size < length {
            size *= 2;
        }
        let mut buffer = vec![Complex::new(0.0, 0.0); size];
        for index in 0..length {
            let window = if length > 1 {
                0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / (length - 1) as f64).cos()
            } else {
                1.0
            };
            buffer[index] = Complex::new(mono[start + index] * window, 0.0);
        }
        FftPlanner::<f64>::new()
            .plan_fft_forward(size)
            .process(&mut buffer);
        let mut sum = 0.0;
        for (bin, value) in buffer.iter().take(size / 2).enumerate() {
            let freq = bin as f64 * SR as f64 / size as f64;
            if freq >= low && freq <= high {
                sum += value.norm_sqr();
            }
        }
        sum
    }

    fn gain_db(ratio: f64) -> f64 {
        10.0 * ratio.max(1e-30).log10()
    }

    fn assert_same(left: &[f32], right: &[f32]) {
        assert_eq!(left.len(), right.len());
        if let Some(index) = left.iter().zip(right).position(|(a, b)| a != b) {
            panic!(
                "first difference at {index}: {} vs {}",
                left[index], right[index]
            );
        }
    }

    fn f0(mono: &[f64], start: usize, frames: usize, low: f64, high: f64) -> f64 {
        let window = &mono[start..start + frames];
        let mean = window.iter().sum::<f64>() / frames as f64;
        let centered: Vec<f64> = window.iter().map(|sample| sample - mean).collect();
        let min_lag = (SR as f64 / high).floor() as usize;
        let max_lag = (SR as f64 / low).ceil() as usize;
        let mut best = (f64::MIN, min_lag);
        for lag in min_lag..=max_lag {
            let mut correlation = 0.0;
            let mut now = 0.0;
            let mut then = 0.0;
            for index in lag..frames {
                correlation += centered[index] * centered[index - lag];
                now += centered[index] * centered[index];
                then += centered[index - lag] * centered[index - lag];
            }
            let score = correlation / ((now * then).sqrt() + SILENCE);
            if score > best.0 {
                best = (score, lag);
            }
        }
        SR as f64 / best.1 as f64
    }

    fn peak_reduction(effect: &mut Plosive, input: &[f32]) -> f64 {
        let mut output = input.to_vec();
        let mut peak = 0.0_f64;
        for chunk in output.chunks_mut(48 * 2) {
            effect.process(chunk, SR);
            peak = peak.max(effect.peak_reduction_db);
        }
        peak
    }

    #[test]
    fn defaults_are_a_bit_exact_no_op() {
        let input = signal(Some(0.8), 220.0, 0.4);
        let mut effect = Plosive::new(SR);
        assert_eq!(effect.name(), "plosive");
        assert_eq!(effect.latency_samples(), 0);
        assert_eq!(effect.tail_samples(), 0);
        assert_eq!(effect.get_param("amount"), Some(0.0));
        let output = run(&mut effect, &input, 480);
        assert_same(&output, &input);
    }

    #[test]
    fn amount_zero_is_bit_identical() {
        let input = signal_with_burst_over_note(110.0);
        let mut effect = Plosive::new(SR);
        effect.set_param("amount", 100.0);
        effect.set_param("amount", 0.0);
        assert_eq!(effect.latency_samples(), 0);
        let output = run(&mut effect, &input, 97);
        assert_same(&output, &input);
        assert_eq!(effect.gain_reduction_db(), 0.0);
    }

    #[test]
    fn s8_burst_is_taken_down_without_touching_the_vowel() {
        let input = signal(Some(1.0), 220.0, 0.5);
        let mut effect = Plosive::new(SR);
        effect.set_param("amount", 100.0);
        let mut output = input.clone();
        let mut peak = 0.0_f64;
        for chunk in output.chunks_mut(48 * 2) {
            effect.process(chunk, SR);
            peak = peak.max(effect.peak_reduction_db);
        }
        assert!(peak >= 6.0, "the detector only reached {peak:.2} dB");

        let input_mono = mono(&input);
        let output_mono = mono(&output);
        let burst_start = (0.05 * SR as f64) as usize;
        let burst_end = burst_start + (0.030 * SR as f64) as usize;
        let low_before = band_energy(&input_mono, burst_start, burst_end, 20.0, 120.0);
        let low_after = band_energy(&output_mono, burst_start, burst_end, 20.0, 120.0);
        let low_reduction = gain_db(low_after / low_before);

        let vowel_start = (0.085 * SR as f64) as usize;
        let body_start = vowel_start + (0.020 * SR as f64) as usize;
        let body_end = vowel_start + (0.300 * SR as f64) as usize;
        let body_before = band_energy(&input_mono, body_start, body_end, 500.0, 2_000.0);
        let body_after = band_energy(&output_mono, body_start, body_end, 500.0, 2_000.0);
        let body_change = gain_db(body_after / body_before);

        let mut added_step: f64 = 0.0;
        for frame in 1..input_mono.len() {
            let input_step = input_mono[frame] - input_mono[frame - 1];
            let output_step = output_mono[frame] - output_mono[frame - 1];
            added_step = added_step.max((output_step - input_step).abs());
        }
        println!(
            "S8: low {low_reduction:.2} dB, body {body_change:.3} dB, peak {peak:.2} dB, added step {added_step:.5}"
        );
        assert!(
            low_reduction <= -6.0,
            "low band only moved {low_reduction:.2} dB"
        );
        assert!(
            body_change.abs() <= 0.3,
            "vowel body moved {body_change:.3} dB"
        );
        assert!(added_step <= 0.012, "added step {added_step:.5}");
    }

    #[test]
    fn vowel_only_never_leaves_unity() {
        for fundamental in [110.0, 220.0, 440.0] {
            let input = signal(None, fundamental, 0.4);
            let mut effect = Plosive::new(SR);
            effect.set_param("amount", 100.0);
            let mut output = input.clone();
            let mut peak = 0.0_f64;
            for chunk in output.chunks_mut(97 * 2) {
                effect.process(chunk, SR);
                peak = peak.max(effect.peak_reduction_db);
            }
            if let Some(index) = output.iter().zip(&input).position(|(a, b)| a != b) {
                panic!("f0 {fundamental} was modified at sample {index}");
            }
            assert_eq!(peak, 0.0, "f0 {fundamental} fired at {peak:.3} dB");
        }
    }

    #[test]
    fn plosive_over_a_sung_note_keeps_the_pitch() {
        let input = signal_with_burst_over_note(110.0);
        let mut effect = Plosive::new(SR);
        effect.set_param("amount", 100.0);
        let peak = peak_reduction(&mut effect, &input);
        assert!(peak >= 6.0, "the detector only reached {peak:.2} dB");
        effect.reset();
        let output = run(&mut effect, &input, 480);
        let input_mono = mono(&input);
        let output_mono = mono(&output);
        let start = (0.085 * SR as f64) as usize + (0.060 * SR as f64) as usize;
        let input_pitch = f0(&input_mono, start, 2_880, 90.0, 140.0);
        let output_pitch = f0(&output_mono, start, 2_880, 90.0, 140.0);
        let cents = 1_200.0 * (output_pitch / input_pitch).log2();
        println!(
            "sung note: input {input_pitch:.3} Hz, output {output_pitch:.3} Hz, {cents:.3} cents"
        );
        assert!(cents.abs() <= 5.0, "pitch moved {cents:.3} cents");
        let burst_start = (0.05 * SR as f64) as usize;
        let burst_end = burst_start + (0.030 * SR as f64) as usize;
        let before = band_energy(&input_mono, burst_start, burst_end, 20.0, 120.0);
        let after = band_energy(&output_mono, burst_start, burst_end, 20.0, 120.0);
        let reduction = gain_db(after / before);
        println!("sung note: low {reduction:.2} dB");
        assert!(reduction <= -6.0, "low band only moved {reduction:.2} dB");
    }

    #[test]
    fn block_size_is_invariant() {
        let input = signal_with_burst_over_note(220.0);
        let mut small = Plosive::new(SR);
        small.set_param("amount", 100.0);
        let mut large = Plosive::new(SR);
        large.set_param("amount", 100.0);
        let a = run(&mut small, &input, 97);
        let b = run(&mut large, &input, 480);
        assert_same(&a, &b);
        assert_ne!(a, input);
    }

    #[test]
    fn reset_is_deterministic() {
        let input = signal_with_burst_over_note(220.0);
        let mut effect = Plosive::new(SR);
        effect.set_param("amount", 100.0);
        let first = run(&mut effect, &input, 97);
        effect.reset();
        let second = run(&mut effect, &input, 97);
        assert_same(&first, &second);
    }

    #[test]
    fn monitor_outputs_only_the_removed_band() {
        let input = signal(Some(1.0), 220.0, 1.0);
        let mut effect = Plosive::new(SR);
        effect.set_param("amount", 100.0);
        effect.set_param("monitor", 1.0);
        let output = run(&mut effect, &input, 480);
        let input_mono = mono(&input);
        let output_mono = mono(&output);
        let burst_start = (0.05 * SR as f64) as usize;
        let burst_end = burst_start + (0.030 * SR as f64) as usize;
        let low = band_energy(&output_mono, burst_start, burst_end, 20.0, 120.0);
        let high = band_energy(&output_mono, burst_start, burst_end, 500.0, 2_000.0);
        println!(
            "monitor: low {low:.3e}, high {high:.3e}, ratio {:.2e}",
            high / low
        );
        assert!(high / low < 1e-2, "monitor is not low-band only");
        let body_start = (0.085 * SR as f64) as usize + (0.600 * SR as f64) as usize;
        let body_end = body_start + (0.100 * SR as f64) as usize;
        let silent = output_mono[body_start..body_end]
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
        assert!(silent < 1e-6, "monitor leaks the steady vowel: {silent}");
        assert!(input_mono.len() == output_mono.len());
    }

    #[test]
    fn parameter_guards_and_clamps() {
        let mut effect = Plosive::new(SR);
        effect.set_param("frequency", 1_000.0);
        assert_eq!(effect.get_param("frequency"), Some(MAX_FREQUENCY_HZ));
        effect.set_param("frequency", 10.0);
        assert_eq!(effect.get_param("frequency"), Some(MIN_FREQUENCY_HZ));
        effect.set_param("threshold", 90.0);
        assert_eq!(effect.get_param("threshold"), Some(MAX_THRESHOLD_DB));
        effect.set_param("release", 1.0);
        assert_eq!(effect.get_param("release"), Some(MIN_RELEASE_MS));
        effect.set_param("amount", 150.0);
        assert_eq!(effect.get_param("amount"), Some(100.0));
        effect.set_param("amount", -4.0);
        assert_eq!(effect.get_param("amount"), Some(0.0));
        assert_eq!(effect.get_param("bogus"), None);
        effect.set_param("bogus", 1.0);
        effect.set_param("amount", f64::NAN);
        assert_eq!(effect.get_param("amount"), Some(0.0));
    }

    fn band_noise(high_hz: f64, rms: f64, frames: usize, seed: u32) -> Vec<f64> {
        let mut high = BiquadFilter::new(FilterType::HighPass, high_hz, 0.707, 0.0, SR);
        let mut samples: Vec<f64> = noise(frames, seed)
            .into_iter()
            .map(|sample| high.process_sample(0, sample))
            .collect();
        let power = samples.iter().map(|s| s * s).sum::<f64>() / frames as f64;
        let scale = if power > 0.0 { rms / power.sqrt() } else { 0.0 };
        for sample in &mut samples {
            *sample *= scale;
        }
        samples
    }

    fn tone_signal(frequency: f64, amplitude: f64, total_s: f64) -> Vec<f32> {
        let frames = (total_s * SR as f64) as usize;
        (0..frames)
            .flat_map(|frame| {
                let value = amplitude
                    * (std::f64::consts::TAU * frequency * frame as f64 / SR as f64).sin();
                [value as f32, value as f32]
            })
            .collect()
    }

    fn sibilant_signal() -> Vec<f32> {
        let frames = (0.5 * SR as f64) as usize;
        let start = (0.05 * SR as f64) as usize;
        let mut mono = vec![0.0; frames];
        let burst = band_noise(6_000.0, 0.1, (0.1 * SR as f64) as usize, 0x51ce_0010);
        for (index, sample) in burst.iter().enumerate() {
            mono[start + index] += sample;
        }
        mono.iter()
            .flat_map(|sample| [*sample as f32, *sample as f32])
            .collect()
    }

    fn thump_signal() -> Vec<f32> {
        let frames = (0.4 * SR as f64) as usize;
        let start = (0.05 * SR as f64) as usize;
        let mut mono = vec![0.0; frames];
        for index in 0..(0.030 * SR as f64) as usize {
            let time = index as f64 / SR as f64;
            let envelope = if time < 0.002 {
                time / 0.002
            } else {
                (1.0 - (time - 0.002) / 0.028).max(0.0)
            };
            mono[start + index] +=
                0.5 * envelope * (std::f64::consts::TAU * 60.0 * index as f64 / SR as f64).sin();
        }
        mono.iter()
            .flat_map(|sample| [*sample as f32, *sample as f32])
            .collect()
    }

    #[test]
    fn detector_measurements() {
        let scenarios: [(&str, Vec<f32>, bool); 9] = [
            ("burst", signal(Some(1.0), 220.0, 0.2), true),
            ("quiet burst", signal(Some(0.1), 220.0, 0.2), true),
            ("burst + note 110", signal_with_burst_over_note(110.0), true),
            ("vowel 110", signal(None, 110.0, 0.4), false),
            ("vowel 220", signal(None, 220.0, 0.4), false),
            ("vowel 440", signal(None, 440.0, 0.4), false),
            ("sibilant 6-9k", sibilant_signal(), false),
            ("steady 1k", tone_signal(1_000.0, 0.3, 0.4), false),
            ("60 Hz thump", thump_signal(), true),
        ];
        for (name, input, expected) in scenarios {
            let mut effect = Plosive::new(SR);
            effect.set_param("amount", 100.0);
            let mut output = input.clone();
            let mut rise: f64 = 0.0;
            let mut share: f64 = 0.0;
            let mut share_while_rising: f64 = 0.0;
            let mut rise_while_sharing: f64 = 0.0;
            let mut peak: f64 = 0.0;
            for chunk in output.chunks_mut(48 * 2) {
                effect.process(chunk, SR);
                rise = rise.max(effect.last_rise_db);
                share = share.max(effect.last_share);
                if effect.last_rise_db >= effect.threshold_db {
                    share_while_rising = share_while_rising.max(effect.last_share);
                }
                if effect.last_share >= LOW_EXCESS_SHARE {
                    rise_while_sharing = rise_while_sharing.max(effect.last_rise_db);
                }
                peak = peak.max(effect.peak_reduction_db);
            }
            println!(
                "{name:>16}: max rise {rise:6.2} dB, max share {share:.3}, share while rising {share_while_rising:.3}, max rise while sharing {rise_while_sharing:6.2} dB, peak {peak:6.2} dB"
            );
            if expected {
                assert!(peak > 6.0, "{name} did not fire");
                assert!(share_while_rising >= LOW_EXCESS_SHARE);
            } else {
                assert_eq!(peak, 0.0, "{name} fired at {peak:.2} dB");
                assert!(
                    share_while_rising < LOW_EXCESS_SHARE,
                    "{name} share {share_while_rising:.3}"
                );
            }
        }
    }

    #[test]
    fn detection_delay_and_depth_measurements() {
        let input = signal(Some(1.0), 220.0, 0.2);
        let burst_start = (0.05 * SR as f64) as usize;
        let mut effect = Plosive::new(SR);
        effect.set_param("amount", 100.0);
        let mut output = input.clone();
        let mut onset = None;
        let mut half = None;
        for frame in 0..input.len() / 2 {
            effect.process(&mut output[frame * 2..frame * 2 + 2], SR);
            if onset.is_none() && effect.gain_reduction_db() > 0.001 {
                onset = Some(frame - burst_start);
            }
            if half.is_none() && effect.gain_reduction_db() >= 6.0 {
                half = Some(frame - burst_start);
            }
        }
        let onset = onset.expect("the burst never engaged");
        let half = half.expect("the burst never reached half depth");
        println!(
            "detection: engages after {onset} samples ({:.2} ms), half depth after {half} samples ({:.2} ms)",
            onset as f64 / SR as f64 * 1_000.0,
            half as f64 / SR as f64 * 1_000.0
        );
        assert!(onset <= 384, "engagement took {onset} samples");
        assert!(half <= 480, "half depth took {half} samples");

        for amount in [100.0, 75.0, 50.0, 25.0] {
            let mut effect = Plosive::new(SR);
            effect.set_param("amount", amount);
            let mut output = input.clone();
            for chunk in output.chunks_mut(48 * 2) {
                effect.process(chunk, SR);
            }
            let input_mono = mono(&input);
            let output_mono = mono(&output);
            let end = burst_start + (0.030 * SR as f64) as usize;
            let before = band_energy(&input_mono, burst_start, end, 20.0, 120.0);
            let after = band_energy(&output_mono, burst_start, end, 20.0, 120.0);
            println!(
                "amount {amount:3.0} %: low band {:.2} dB",
                gain_db(after / before)
            );
        }
    }

    #[test]
    fn shelf_response_measurements() {
        for amount in [100.0, 50.0] {
            let coefficients = BiquadCoefficients::design(
                FilterType::LowShelf,
                DEFAULT_FREQUENCY_HZ,
                SHELF_Q,
                -MAX_DEPTH_DB * amount * 0.01,
                SR as f64,
            );
            let mut row = String::new();
            for frequency in [
                30.0, 60.0, 90.0, 120.0, 150.0, 250.0, 500.0, 1_000.0, 2_000.0,
            ] {
                let response = coefficients.response_db(frequency, SR as f64);
                row.push_str(&format!(" {frequency:.0} Hz {response:+.2} dB;"));
            }
            println!("shelf at {amount:.0} %:{row}");
            let at_60 = coefficients.response_db(60.0, SR as f64);
            let at_120 = coefficients.response_db(120.0, SR as f64);
            let at_500 = coefficients.response_db(500.0, SR as f64);
            let at_2k = coefficients.response_db(2_000.0, SR as f64);
            assert!(at_60 <= -10.0 || amount < 100.0, "60 Hz only {at_60:.2} dB");
            assert!(
                at_120 <= -5.5 || amount < 100.0,
                "120 Hz only {at_120:.2} dB"
            );
            assert!(at_500 >= -0.4, "500 Hz moved {at_500:.2} dB");
            assert!(at_2k >= -0.05, "2 kHz moved {at_2k:.2} dB");
            for frequency in [
                30.0, 60.0, 90.0, 120.0, 150.0, 250.0, 500.0, 1_000.0, 2_000.0,
            ] {
                assert!(
                    coefficients.response_db(frequency, SR as f64) <= 0.001,
                    "shelf boosts {frequency} Hz"
                );
            }
        }
    }
}
