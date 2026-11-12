//! Resonance suppressor: many-band spectral dynamics over a 2048/256 STFT.
//!
//! Each bin is reduced only where its power stands above a one-third-octave
//! local mean. The reduction curve becomes a 2048-tap minimum-phase FIR
//! through the cepstrum, so a narrow notch has no pre-ring.
//!
//! The frame is completed one hop before its first output sample, which makes
//! the reported latency `FFT_SIZE + HOP` (2304 samples, 48 ms at 48 kHz).

use std::f64::consts::{LN_10, TAU};
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use super::AudioEffect;

/// STFT size of the resonance suppressor.
pub const FFT_SIZE: usize = 2048;
/// Samples between STFT frames.
pub const HOP: usize = 256;
/// Reported latency: one analysis frame plus one hop of synthesis margin.
pub const LATENCY: usize = FFT_SIZE + HOP;

const BINS: usize = FFT_SIZE / 2 + 1;
/// Sum of `w^2` over the eight overlapping Hann frames.
const OLA_SUM: f64 = 3.0;
const MIN_LEVEL: f64 = 1e-30;
/// Log-spaced analysis bands spanning 40 Hz to 16 kHz, as a band-count proxy.
const MIN_BANDS: f64 = 12.0;
const MAX_BANDS: f64 = 52.0;
const BAND_LOW_HZ: f64 = 40.0;
const BAND_HIGH_HZ: f64 = 16_000.0;
/// Largest reduction step between adjacent FFT bins (23 Hz at 48 kHz).
const SLOPE_DB_PER_BIN: f64 = 2.0;

/// Lowest detection threshold.
pub const MIN_THRESHOLD_DB: f64 = -12.0;
/// Highest detection threshold.
pub const MAX_THRESHOLD_DB: f64 = 18.0;
/// Lowest maximum reduction.
pub const MIN_DEPTH_DB: f64 = 0.0;
/// Highest maximum reduction.
pub const MAX_DEPTH_DB: f64 = 24.0;
/// Fastest reduction attack.
pub const MIN_ATTACK_MS: f64 = 1.0;
/// Slowest reduction attack.
pub const MAX_ATTACK_MS: f64 = 100.0;
/// Fastest reduction release.
pub const MIN_RELEASE_MS: f64 = 20.0;
/// Slowest reduction release.
pub const MAX_RELEASE_MS: f64 = 1_000.0;

/// Parameters whose DSL value is a boolean switch.
pub fn switch_param(name: &str) -> bool {
    name == "enabled"
}

/// A spectral resonance suppressor with cepstral minimum-phase filtering.
pub struct Resonance {
    sample_rate: u32,
    threshold_db: f64,
    depth_db: f64,
    selectivity: f64,
    attack_ms: f64,
    release_ms: f64,
    mix: f64,
    enabled: bool,
    attack_coeff: f64,
    release_coeff: f64,
    bin_hz: f64,
    env_lo: Vec<usize>,
    env_hi: Vec<usize>,
    window: Vec<f64>,
    forward: Arc<dyn Fft<f64>>,
    inverse: Arc<dyn Fft<f64>>,
    scratch: Vec<Complex<f64>>,
    spec_l: Vec<Complex<f64>>,
    spec_r: Vec<Complex<f64>>,
    cepstrum: Vec<Complex<f64>>,
    filter: Vec<Complex<f64>>,
    real: Vec<f64>,
    power: Vec<f64>,
    prefix: Vec<f64>,
    reduction: Vec<f64>,
    target: Vec<f64>,
    dilated: Vec<f64>,
    in_l: Vec<f64>,
    in_r: Vec<f64>,
    in_write: usize,
    ola_l: Vec<f64>,
    ola_r: Vec<f64>,
    dry_l: Vec<f64>,
    dry_r: Vec<f64>,
    dry_pos: usize,
    out_l: Vec<f64>,
    out_r: Vec<f64>,
    out_read: usize,
    out_pending: usize,
    hop_count: usize,
    total: usize,
    last_gr_db: f64,
    peak_gr_db: f64,
}

impl Resonance {
    /// Prepare the STFT and filter buffers at the sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let mut planner = FftPlanner::<f64>::new();
        let forward = planner.plan_fft_forward(FFT_SIZE);
        let inverse = planner.plan_fft_inverse(FFT_SIZE);
        let scratch_len = forward
            .get_inplace_scratch_len()
            .max(inverse.get_inplace_scratch_len());
        let window: Vec<f64> = (0..FFT_SIZE)
            .map(|index| 0.5 - 0.5 * (TAU * index as f64 / FFT_SIZE as f64).cos())
            .collect();
        let mut effect = Self {
            sample_rate,
            threshold_db: 6.0,
            depth_db: 9.0,
            selectivity: 0.35,
            attack_ms: 10.0,
            release_ms: 150.0,
            mix: 1.0,
            enabled: true,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            bin_hz: sample_rate as f64 / FFT_SIZE as f64,
            env_lo: vec![1; BINS],
            env_hi: vec![1; BINS],
            window,
            forward,
            inverse,
            scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            spec_l: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            spec_r: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            cepstrum: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            filter: vec![Complex::new(1.0, 0.0); FFT_SIZE],
            real: vec![0.0; FFT_SIZE],
            power: vec![0.0; BINS],
            prefix: vec![0.0; BINS + 1],
            reduction: vec![0.0; BINS],
            target: vec![0.0; BINS],
            dilated: vec![0.0; BINS],
            in_l: vec![0.0; FFT_SIZE],
            in_r: vec![0.0; FFT_SIZE],
            in_write: 0,
            ola_l: vec![0.0; LATENCY],
            ola_r: vec![0.0; LATENCY],
            dry_l: vec![0.0; LATENCY],
            dry_r: vec![0.0; LATENCY],
            dry_pos: 0,
            out_l: vec![0.0; HOP],
            out_r: vec![0.0; HOP],
            out_read: 0,
            out_pending: 0,
            hop_count: 0,
            total: 0,
            last_gr_db: 0.0,
            peak_gr_db: 0.0,
        };
        effect.update_ballistics();
        effect.update_bands();
        effect
    }

    fn bypass(&self) -> bool {
        !self.enabled || self.depth_db <= 0.0 || self.mix <= 0.0
    }

    /// Bytes held by the construction-time buffers; the audio path allocates none.
    pub fn memory_bytes(&self) -> usize {
        let f = std::mem::size_of::<f64>();
        let c = std::mem::size_of::<Complex<f64>>();
        (self.env_lo.len()
            + self.env_hi.len()
            + self.window.len()
            + self.real.len()
            + self.power.len()
            + self.prefix.len()
            + self.reduction.len()
            + self.target.len()
            + self.dilated.len()
            + self.in_l.len()
            + self.in_r.len()
            + self.ola_l.len()
            + self.ola_r.len()
            + self.dry_l.len()
            + self.dry_r.len()
            + self.out_l.len()
            + self.out_r.len())
            * f
            + (self.scratch.len()
                + self.spec_l.len()
                + self.spec_r.len()
                + self.cepstrum.len()
                + self.filter.len())
                * c
    }

    fn update_ballistics(&mut self) {
        let frames_per_second = self.sample_rate as f64 / HOP as f64;
        let attack_tau = (self.attack_ms * 0.001 * frames_per_second).max(1e-9);
        let release_tau = (self.release_ms * 0.001 * frames_per_second).max(1e-9);
        self.attack_coeff = 1.0 - (-1.0 / attack_tau).exp();
        self.release_coeff = 1.0 - (-1.0 / release_tau).exp();
    }

    fn update_bands(&mut self) {
        let bands = MIN_BANDS + self.selectivity * (MAX_BANDS - MIN_BANDS);
        let width_octaves = (BAND_HIGH_HZ / BAND_LOW_HZ).log2() / bands.round().max(1.0);
        let low_factor = 2f64.powf(-0.5 * width_octaves);
        let high_factor = 2f64.powf(0.5 * width_octaves);
        for bin in 0..BINS {
            let frequency = bin as f64 * self.bin_hz;
            let low = (frequency * low_factor / self.bin_hz).floor() as isize;
            let high = (frequency * high_factor / self.bin_hz).ceil() as isize;
            let low = low.clamp(1, BINS as isize - 2) as usize;
            let high = high.clamp(1, BINS as isize - 2) as usize;
            self.env_lo[bin] = low;
            self.env_hi[bin] = high.max(low);
        }
    }

    fn clear_state(&mut self) {
        self.in_l.fill(0.0);
        self.in_r.fill(0.0);
        self.ola_l.fill(0.0);
        self.ola_r.fill(0.0);
        self.dry_l.fill(0.0);
        self.dry_r.fill(0.0);
        self.out_l.fill(0.0);
        self.out_r.fill(0.0);
        self.reduction.fill(0.0);
        self.target.fill(0.0);
        self.dilated.fill(0.0);
        self.in_write = 0;
        self.dry_pos = 0;
        self.out_read = 0;
        self.out_pending = 0;
        self.hop_count = 0;
        self.total = 0;
        self.last_gr_db = 0.0;
        self.peak_gr_db = 0.0;
    }

    /// Minimum-phase response from the current `dilated` reduction curve.
    fn design_filter(&mut self) {
        let scale = -LN_10 / 20.0;
        for bin in 0..BINS {
            let log_magnitude = self.dilated[bin] * scale;
            self.cepstrum[bin] = Complex::new(log_magnitude, 0.0);
        }
        for bin in 1..BINS - 1 {
            self.cepstrum[FFT_SIZE - bin] = Complex::new(self.cepstrum[bin].re, 0.0);
        }
        self.inverse
            .process_with_scratch(&mut self.cepstrum, &mut self.scratch);
        for index in 0..FFT_SIZE {
            self.real[index] = self.cepstrum[index].re / FFT_SIZE as f64;
        }
        for index in 1..FFT_SIZE / 2 {
            self.real[index] *= 2.0;
        }
        for index in FFT_SIZE / 2 + 1..FFT_SIZE {
            self.real[index] = 0.0;
        }
        for index in 0..FFT_SIZE {
            self.cepstrum[index] = Complex::new(self.real[index], 0.0);
        }
        self.forward
            .process_with_scratch(&mut self.cepstrum, &mut self.scratch);
        for index in 0..FFT_SIZE {
            let value = self.cepstrum[index];
            let magnitude = value.re.exp();
            let (sin, cos) = value.im.sin_cos();
            self.filter[index] = Complex::new(magnitude * cos, magnitude * sin);
        }
    }

    fn process_frame(&mut self) {
        let start = self.in_write;
        for index in 0..FFT_SIZE {
            let source = (start + index) % FFT_SIZE;
            let window = self.window[index];
            self.spec_l[index] = Complex::new(self.in_l[source] * window, 0.0);
            self.spec_r[index] = Complex::new(self.in_r[source] * window, 0.0);
        }
        self.forward
            .process_with_scratch(&mut self.spec_l, &mut self.scratch);
        self.forward
            .process_with_scratch(&mut self.spec_r, &mut self.scratch);

        self.prefix[0] = 0.0;
        for bin in 0..BINS {
            let power = 0.5 * (self.spec_l[bin].norm_sqr() + self.spec_r[bin].norm_sqr());
            self.power[bin] = power;
            self.prefix[bin + 1] = self.prefix[bin] + power;
        }
        for bin in 0..BINS {
            let low = self.env_lo[bin];
            let high = self.env_hi[bin];
            let total = self.prefix[high + 1] - self.prefix[low];
            let center_low = bin.saturating_sub(2).max(low);
            let center_high = (bin + 2).min(high);
            let center = self.prefix[center_high + 1] - self.prefix[center_low];
            let count = (high + 1 - low) - (center_high + 1 - center_low);
            let envelope = if count >= 3 {
                (total - center) / count as f64
            } else {
                total / (high + 1 - low) as f64
            };
            let prominence =
                10.0 * (self.power[bin].max(MIN_LEVEL) / envelope.max(MIN_LEVEL)).log10();
            let target = (prominence - self.threshold_db).clamp(0.0, self.depth_db);
            let coefficient = if target > self.reduction[bin] {
                self.attack_coeff
            } else {
                self.release_coeff
            };
            self.reduction[bin] += (target - self.reduction[bin]) * coefficient;
            self.target[bin] = self.reduction[bin].max(0.0);
        }
        self.target[0] = 0.0;
        self.target[BINS - 1] = 0.0;
        for bin in 1..BINS - 1 {
            self.dilated[bin] = self.target[bin - 1]
                .max(self.target[bin])
                .max(self.target[bin + 1]);
        }
        self.dilated[0] = 0.0;
        self.dilated[BINS - 1] = 0.0;
        for bin in 1..BINS {
            self.dilated[bin] = self.dilated[bin].max(self.dilated[bin - 1] - SLOPE_DB_PER_BIN);
        }
        for bin in (0..BINS - 1).rev() {
            self.dilated[bin] = self.dilated[bin].max(self.dilated[bin + 1] - SLOPE_DB_PER_BIN);
        }
        self.last_gr_db = self.dilated.iter().copied().fold(0.0, f64::max);
        self.peak_gr_db = self.peak_gr_db.max(self.last_gr_db);

        self.design_filter();
        for bin in 0..FFT_SIZE {
            self.spec_l[bin] *= self.filter[bin];
            self.spec_r[bin] *= self.filter[bin];
        }
        self.inverse
            .process_with_scratch(&mut self.spec_l, &mut self.scratch);
        self.inverse
            .process_with_scratch(&mut self.spec_r, &mut self.scratch);

        let scale = 1.0 / (FFT_SIZE as f64 * OLA_SUM);
        let frame_start = self.total - FFT_SIZE;
        for index in 0..FFT_SIZE {
            let slot = (frame_start + index) % LATENCY;
            let window = self.window[index];
            self.ola_l[slot] += self.spec_l[index].re * scale * window;
            self.ola_r[slot] += self.spec_r[index].re * scale * window;
        }
    }

    fn process_hop(&mut self) {
        self.total += HOP;
        if self.total >= FFT_SIZE {
            self.process_frame();
        }
        let base = self.total as isize - LATENCY as isize;
        debug_assert_eq!(self.out_pending, 0);
        self.out_read = 0;
        if base >= 0 {
            let base = base as usize;
            for index in 0..HOP {
                let slot = (base + index) % LATENCY;
                self.out_l[index] = self.ola_l[slot];
                self.out_r[index] = self.ola_r[slot];
                self.ola_l[slot] = 0.0;
                self.ola_r[slot] = 0.0;
            }
            self.out_pending = HOP;
        }
    }
}

impl AudioEffect for Resonance {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        if self.bypass() {
            return;
        }
        for frame in buffer.as_chunks_mut::<2>().0 {
            let input_l = frame[0] as f64;
            let input_r = frame[1] as f64;
            let dry_index = self.dry_pos;
            let dry_l = self.dry_l[dry_index];
            let dry_r = self.dry_r[dry_index];
            self.dry_l[dry_index] = input_l;
            self.dry_r[dry_index] = input_r;
            self.dry_pos = (dry_index + 1) % LATENCY;
            if self.out_pending > 0 {
                let index = self.out_read;
                let wet_l = self.out_l[index];
                let wet_r = self.out_r[index];
                let mix = self.mix;
                frame[0] = (dry_l + (wet_l - dry_l) * mix) as f32;
                frame[1] = (dry_r + (wet_r - dry_r) * mix) as f32;
                self.out_read += 1;
                self.out_pending -= 1;
            } else {
                frame[0] = 0.0;
                frame[1] = 0.0;
            }
            self.in_l[self.in_write] = input_l;
            self.in_r[self.in_write] = input_r;
            self.in_write = (self.in_write + 1) % FFT_SIZE;
            self.hop_count += 1;
            if self.hop_count == HOP {
                self.hop_count = 0;
                self.process_hop();
            }
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        let was_bypass = self.bypass();
        match name {
            "threshold" => self.threshold_db = value.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB),
            "depth" => self.depth_db = value.clamp(MIN_DEPTH_DB, MAX_DEPTH_DB),
            "selectivity" => {
                self.selectivity = (value * 0.01).clamp(0.0, 1.0);
                self.update_bands();
            }
            "attack" => {
                self.attack_ms = value.clamp(MIN_ATTACK_MS, MAX_ATTACK_MS);
                self.update_ballistics();
            }
            "release" => {
                self.release_ms = value.clamp(MIN_RELEASE_MS, MAX_RELEASE_MS);
                self.update_ballistics();
            }
            "mix" => self.mix = value.clamp(0.0, 1.0),
            "enabled" => self.enabled = value >= 0.5,
            _ => return,
        }
        if was_bypass != self.bypass() {
            self.clear_state();
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "threshold" => Some(self.threshold_db),
            "depth" => Some(self.depth_db),
            "selectivity" => Some(self.selectivity * 100.0),
            "attack" => Some(self.attack_ms),
            "release" => Some(self.release_ms),
            "mix" => Some(self.mix),
            "enabled" => Some(if self.enabled { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.clear_state();
    }

    fn latency_samples(&self) -> u32 {
        if self.bypass() { 0 } else { LATENCY as u32 }
    }

    fn tail_samples(&self) -> u32 {
        (LATENCY + FFT_SIZE) as u32
    }

    fn gain_reduction_db(&self) -> f64 {
        self.last_gr_db
    }

    fn max_gain_reduction_db(&self) -> f64 {
        self.peak_gr_db
    }

    fn name(&self) -> &str {
        "resonance"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const RATE: u32 = 48_000;
    const DEPTH_DB: f64 = 9.0;
    const THRESHOLD_DB: f64 = 6.0;
    const RING_HZ: f64 = 3_000.0;
    const NEIGHBOUR_HZ: f64 = 2_500.0;

    fn tone_amplitude(samples: &[f32], frequency: f64, start: usize, frames: usize) -> f64 {
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for index in 0..frames {
            let time = (start + index) as f64 / RATE as f64;
            let value = samples[(start + index) * 2] as f64;
            let phase = TAU * frequency * time;
            real += value * phase.cos();
            imaginary += value * phase.sin();
        }
        2.0 * (real * real + imaginary * imaginary).sqrt() / frames as f64
    }

    fn db(value: f64) -> f64 {
        20.0 * value.max(1e-30).log10()
    }

    /// White bed plus a 3 kHz ring and a quiet 2.5 kHz neighbour.
    fn acceptance_signal(frames: usize) -> Vec<f32> {
        let mut output = vec![0.0f32; frames * 2];
        let mut state = 0x1234_5678u32;
        for index in 0..frames {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let noise = (state as f64 / u32::MAX as f64 * 2.0 - 1.0) * 0.08 * 1.732;
            let time = index as f64 / RATE as f64;
            let ring = 0.2 * (TAU * RING_HZ * time).sin();
            let neighbour = 0.003 * (TAU * NEIGHBOUR_HZ * time).sin();
            let value = (noise + ring + neighbour) as f32;
            output[index * 2] = value;
            output[index * 2 + 1] = value;
        }
        output
    }

    fn configured() -> Resonance {
        let mut effect = Resonance::new(RATE);
        effect.set_param("threshold", THRESHOLD_DB);
        effect.set_param("depth", DEPTH_DB);
        effect
    }

    #[test]
    fn a_ring_is_reduced_by_the_requested_depth_and_the_neighbour_stays() {
        let frames = 4 * RATE as usize;
        let input = acceptance_signal(frames);
        let mut output = input.clone();
        let mut effect = configured();
        effect.process(&mut output, RATE);

        let start = RATE as usize;
        let measure = RATE as usize * 5 / 2;
        let ring_in = tone_amplitude(&input, RING_HZ, start, measure);
        let ring_out = tone_amplitude(&output, RING_HZ, start, measure);
        let reduction = db(ring_in) - db(ring_out);
        let neighbour_in = tone_amplitude(&input, NEIGHBOUR_HZ, start, measure);
        let neighbour_out = tone_amplitude(&output, NEIGHBOUR_HZ, start, measure);
        let moved = db(neighbour_out) - db(neighbour_in);
        println!(
            "ring reduction {reduction:.3} dB, neighbour {moved:.3} dB, meter {:.3} dB",
            effect.gain_reduction_db()
        );
        assert!(
            reduction >= DEPTH_DB - 0.05,
            "ring reduction {reduction:.3} dB is below the requested {DEPTH_DB} dB"
        );
        assert!(
            moved.abs() < 1.5,
            "neighbour moved {moved:.3} dB; the limit is 1.5 dB"
        );
        assert_eq!(effect.latency_samples(), LATENCY as u32);
        assert!(effect.gain_reduction_db() >= DEPTH_DB - 0.05);
    }

    #[test]
    fn zero_mix_and_zero_depth_are_exact_bypasses_with_zero_latency() {
        let input = acceptance_signal(RATE as usize);
        for (depth, mix) in [(DEPTH_DB, 0.0), (0.0, 1.0), (0.0, 0.0)] {
            let mut effect = configured();
            effect.set_param("depth", depth);
            effect.set_param("mix", mix);
            let mut output = input.clone();
            effect.process(&mut output, RATE);
            assert_eq!(output, input, "depth {depth}, mix {mix}");
            assert_eq!(effect.latency_samples(), 0, "depth {depth}, mix {mix}");
            assert_eq!(effect.gain_reduction_db(), 0.0);
        }
        let mut effect = configured();
        effect.set_param("enabled", 0.0);
        let mut output = input.clone();
        effect.process(&mut output, RATE);
        assert_eq!(output, input);
        assert_eq!(effect.latency_samples(), 0);
    }

    #[test]
    fn the_reported_latency_is_one_window_plus_one_hop() {
        let frames = RATE as usize;
        let mut impulse = vec![0.0f32; frames * 2];
        impulse[2_000 * 2] = 1.0;
        impulse[2_000 * 2 + 1] = 1.0;
        let mut output = impulse.clone();
        let mut effect = configured();
        effect.set_param("threshold", MAX_THRESHOLD_DB);
        effect.process(&mut output, RATE);
        assert_eq!(effect.latency_samples(), LATENCY as u32);
        let onset = output
            .chunks_exact(2)
            .position(|frame| frame[0].abs() > 1e-9)
            .expect("delayed impulse");
        assert_eq!(onset, 2_000 + LATENCY);
        for frame in 0..2_000 {
            assert!(output[frame * 2].abs() < 1e-12, "pre-ring at frame {frame}");
        }
        assert!((output[onset * 2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_minimum_phase_filter_matches_the_gain_curve() {
        let mut effect = Resonance::new(RATE);
        for (index, gain) in effect.dilated.iter_mut().enumerate() {
            *gain = if (100..200).contains(&index) {
                6.0
            } else {
                0.0
            };
        }
        effect.design_filter();
        for bin in [50usize, 100, 150, 199, 250, 400] {
            let expected = 10f64.powf(-effect.dilated[bin] / 20.0);
            let magnitude = effect.filter[bin].norm();
            assert!(
                (magnitude - expected).abs() < 1e-9,
                "bin {bin}: {magnitude} vs {expected}"
            );
        }
        for bin in [100usize, 150, 199] {
            let mirror = FFT_SIZE - bin;
            assert!((effect.filter[bin].re - effect.filter[mirror].re).abs() < 1e-9);
            assert!((effect.filter[bin].im + effect.filter[mirror].im).abs() < 1e-9);
        }

        let mut impulse = effect.filter.clone();
        effect
            .inverse
            .process_with_scratch(&mut impulse, &mut effect.scratch);
        let mut energy = 0.0;
        let mut moment = 0.0;
        let mut front = 0.0;
        for (index, value) in impulse.iter().enumerate() {
            let sample = (value.re / FFT_SIZE as f64).powi(2);
            energy += sample;
            moment += sample * index as f64;
            if index < FFT_SIZE / 8 {
                front += sample;
            }
        }
        let centroid = moment / energy;
        println!(
            "minimum-phase centroid {centroid:.1} of {} samples, front energy {:.3}",
            FFT_SIZE,
            front / energy
        );
        assert!(
            centroid < FFT_SIZE as f64 / 4.0,
            "centroid {centroid} is too close to linear phase"
        );
        assert!(impulse[0].re > 0.0);
        assert!(front / energy > 0.5);
    }

    #[test]
    fn block_sizes_do_not_change_the_result() {
        let frames = 2 * RATE as usize;
        let input = acceptance_signal(frames);
        let mut whole = input.clone();
        let mut a = configured();
        a.process(&mut whole, RATE);

        let mut chunked = input.clone();
        let mut b = configured();
        let mut offset = 0;
        let sizes = [64usize, 1_000, 2_000, 64, 4_000, 1_000];
        let mut round = 0;
        while offset < frames {
            let end = (offset + sizes[round % sizes.len()]).min(frames);
            b.process(&mut chunked[offset * 2..end * 2], RATE);
            offset = end;
            round += 1;
        }
        assert_eq!(offset, frames);
        assert_eq!(whole, chunked);
    }

    #[test]
    fn mix_is_a_latency_aligned_crossfade() {
        let frames = RATE as usize;
        let mut impulse = vec![0.0f32; frames * 2];
        impulse[2_000 * 2] = 1.0;
        impulse[2_000 * 2 + 1] = 1.0;
        let mut output = impulse.clone();
        let mut effect = configured();
        effect.set_param("threshold", MAX_THRESHOLD_DB);
        effect.set_param("mix", 0.5);
        effect.process(&mut output, RATE);
        assert_eq!(effect.latency_samples(), LATENCY as u32);
        assert!((output[(2_000 + LATENCY) * 2] - 1.0).abs() < 1e-6);

        let input = acceptance_signal(frames);
        let mut output = input.clone();
        let mut effect = configured();
        effect.set_param("mix", 0.5);
        effect.process(&mut output, RATE);
        let start = RATE as usize / 2;
        let frames = RATE as usize / 2;
        let reduction = db(tone_amplitude(&input, RING_HZ, start, frames))
            - db(tone_amplitude(&output, RING_HZ, start, frames));
        let expected = -db(0.5 * (1.0 + 10f64.powf(-DEPTH_DB / 20.0)));
        assert!(
            (reduction - expected).abs() < 0.15,
            "50 % mix reduced the ring by {reduction:.3} dB; law says {expected:.3} dB"
        );
    }

    #[test]
    fn two_passes_are_identical_and_reset_restores_the_state() {
        let frames = RATE as usize;
        let input = acceptance_signal(frames);
        let mut first = input.clone();
        let mut effect = configured();
        effect.process(&mut first, RATE);
        let mut second = input.clone();
        let mut other = configured();
        other.process(&mut second, RATE);
        assert_eq!(first, second);

        effect.reset();
        let mut third = input.clone();
        effect.process(&mut third, RATE);
        assert_eq!(first, third);
    }

    #[test]
    fn selectivity_changes_the_envelope_width() {
        let mut wide = Resonance::new(RATE);
        wide.set_param("selectivity", 0.0);
        let mut narrow = Resonance::new(RATE);
        narrow.set_param("selectivity", 100.0);
        let bin = 128;
        let wide_width = wide.env_hi[bin] - wide.env_lo[bin];
        let narrow_width = narrow.env_hi[bin] - narrow.env_lo[bin];
        assert!(wide_width > narrow_width);
        assert!(wide_width >= 25);
        assert!(narrow_width >= 8);
    }

    #[test]
    fn extreme_settings_stay_finite_and_bounded() {
        let frames = RATE as usize;
        let mut input = vec![0.0f32; frames * 2];
        for index in 0..frames {
            let value = (0.7 * (TAU * 9_000.0 * index as f64 / RATE as f64).sin()) as f32;
            input[index * 2] = value;
            input[index * 2 + 1] = -value;
        }
        for selectivity in [0.0, 50.0, 100.0] {
            let mut effect = Resonance::new(RATE);
            effect.set_param("threshold", MIN_THRESHOLD_DB);
            effect.set_param("depth", MAX_DEPTH_DB);
            effect.set_param("selectivity", selectivity);
            effect.set_param("attack", MIN_ATTACK_MS);
            effect.set_param("release", MIN_RELEASE_MS);
            let mut output = input.clone();
            effect.process(&mut output, RATE);
            assert!(output.iter().all(|value| value.is_finite()));
            assert!(output.iter().all(|value| value.abs() <= 2.0));
            assert!(effect.gain_reduction_db() <= MAX_DEPTH_DB);
        }
    }

    fn measure(depth: f64, threshold: f64, selectivity: f64) -> (f64, f64) {
        let input = acceptance_signal(4 * RATE as usize);
        let mut output = input.clone();
        let mut effect = Resonance::new(RATE);
        effect.set_param("depth", depth);
        effect.set_param("threshold", threshold);
        effect.set_param("selectivity", selectivity);
        effect.process(&mut output, RATE);
        let start = RATE as usize;
        let frames = RATE as usize * 5 / 2;
        let ring = db(tone_amplitude(&input, RING_HZ, start, frames))
            - db(tone_amplitude(&output, RING_HZ, start, frames));
        let neighbour = db(tone_amplitude(&output, NEIGHBOUR_HZ, start, frames))
            - db(tone_amplitude(&input, NEIGHBOUR_HZ, start, frames));
        (ring, neighbour)
    }

    #[test]
    #[ignore]
    fn measurements_for_docs() {
        for depth in [3.0, 6.0, 9.0, 12.0, 15.0, 24.0] {
            let (ring, neighbour) = measure(depth, 6.0, 35.0);
            println!("depth {depth:>4.1} dB: ring -{ring:.3} dB, neighbour {neighbour:+.3} dB");
        }
        for threshold in [0.0, 3.0, 6.0, 9.0, 12.0] {
            let (ring, neighbour) = measure(9.0, threshold, 35.0);
            println!(
                "threshold {threshold:>4.1} dB: ring -{ring:.3} dB, neighbour {neighbour:+.3} dB"
            );
        }
        for selectivity in [0.0, 35.0, 100.0] {
            let (ring, neighbour) = measure(9.0, 6.0, selectivity);
            println!(
                "selectivity {selectivity:>5.1} %: ring -{ring:.3} dB, neighbour {neighbour:+.3} dB"
            );
        }

        let mut notch = Resonance::new(RATE);
        for (index, gain) in notch.dilated.iter_mut().enumerate() {
            *gain = if (120..=136).contains(&index) {
                9.0
            } else {
                0.0
            };
        }
        notch.design_filter();
        let mut impulse = notch.filter.clone();
        notch
            .inverse
            .process_with_scratch(&mut impulse, &mut notch.scratch);
        let mut energy = 0.0;
        let mut moment = 0.0;
        let mut cumulative = [0.0f64; 4];
        for (index, value) in impulse.iter().enumerate() {
            let sample = (value.re / FFT_SIZE as f64).powi(2);
            energy += sample;
            moment += sample * index as f64;
            for (slot, limit) in [64usize, 128, 256, 512].iter().enumerate() {
                if index < *limit {
                    cumulative[slot] += sample;
                }
            }
        }
        println!(
            "notch 9 dB at 3 kHz: centroid {:.1} taps, energy in first 64/128/256/512 taps {:.3}/{:.3}/{:.3}/{:.3}",
            moment / energy,
            cumulative[0] / energy,
            cumulative[1] / energy,
            cumulative[2] / energy,
            cumulative[3] / energy,
        );

        let seconds = 8.0;
        let frames = (RATE as f64 * seconds) as usize;
        let input = acceptance_signal(frames);
        let mut effect = configured();
        let mut output = input.clone();
        let started = Instant::now();
        effect.process(&mut output, RATE);
        let elapsed = started.elapsed().as_secs_f64();
        println!(
            "cpu {:.5} s per audio second ({:.3}x realtime); memory {} bytes; latency {} samples",
            elapsed / seconds,
            elapsed / seconds,
            effect.memory_bytes(),
            effect.latency_samples()
        );

        let mut bypass = configured();
        bypass.set_param("mix", 0.0);
        let mut copied = input.clone();
        let started = Instant::now();
        bypass.process(&mut copied, RATE);
        println!(
            "bypass copy {:.6} s per audio second; latency {} samples; identical {}",
            started.elapsed().as_secs_f64() / seconds,
            bypass.latency_samples(),
            copied == input
        );
    }
}
