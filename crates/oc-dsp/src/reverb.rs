//! Schroeder-Moorer reverb — 8 parallel comb filters + 4 series allpass.
//!
//! The long-form controls (modulation, early/late, cuts, width) are additive:
//! their defaults leave the original Schroeder-Moorer signal path untouched.

use super::AudioEffect;
use super::biquad::{BiquadFilter, FilterType};

const NUM_COMBS: usize = 8;
const NUM_ALLPASS: usize = 4;

// Delay lengths in samples at 44100 Hz (prime-ish for minimal coloration)
const COMB_LENGTHS: [usize; NUM_COMBS] = [1557, 1617, 1491, 1422, 1277, 1356, 1188, 1116];
const ALLPASS_LENGTHS: [usize; NUM_ALLPASS] = [225, 556, 441, 341];

const MODULATION_RATE_HZ: f64 = 0.35;
const MODULATION_MAX_SECONDS: f64 = 0.002;
/// A 0.35 Hz LFO does not need per-sample offsets.
const MODULATION_CONTROL_SAMPLES: u32 = 32;
const EARLY_MAX_SECONDS: f64 = 0.05;
const EARLY_LEFT: [(f64, f32); 4] = [(0.007, 0.50), (0.013, 0.35), (0.019, 0.25), (0.029, 0.15)];
const EARLY_RIGHT: [(f64, f32); 4] = [(0.009, 0.45), (0.015, 0.32), (0.021, 0.23), (0.031, 0.14)];

#[test]
fn typed_decay_uses_each_comb_delay_not_clamped_feedback() {
    for sample_rate in [44100, 48000, 96000] {
        let mut reverb = Reverb::new(sample_rate);
        for seconds in [0.8, 2.4] {
            reverb.set_decay_seconds(seconds);
            for comb in reverb.combs_l.iter().chain(reverb.combs_r.iter()) {
                let loops = seconds * sample_rate as f64 / comb.buffer.len() as f64;
                assert!(((comb.feedback as f64).powf(loops) - 0.001).abs() < 0.000001);
            }
        }
    }
}

/// A Schroeder-Moorer reverb with eight combs and four allpasses.
pub struct Reverb {
    combs_l: [CombFilter; NUM_COMBS],
    combs_r: [CombFilter; NUM_COMBS],
    allpass_l: [AllpassFilter; NUM_ALLPASS],
    allpass_r: [AllpassFilter; NUM_ALLPASS],
    decay: f64,
    decay_seconds: Option<f64>,
    damping: f64,
    mix: f64,
    modulation: f64,
    early_late: f64,
    stereo_width: f64,
    low_cut_hz: f64,
    high_cut_hz: f64,
    low_cut: [BiquadFilter; 2],
    high_cut: [BiquadFilter; 2],
    lfo_phase: f64,
    lfo_offsets: [f64; NUM_COMBS],
    lfo_countdown: u32,
    predelay_samples: usize,
    predelay_buf_l: Vec<f32>,
    predelay_buf_r: Vec<f32>,
    predelay_pos: usize,
    early_buf_l: Vec<f32>,
    early_buf_r: Vec<f32>,
    early_pos: usize,
    sample_rate: u32,
}

impl Reverb {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let scale = sample_rate as f64 / 44100.0;
        let decay = 0.8;
        let damping = 0.3;

        let make_combs = |offset: usize| -> [CombFilter; NUM_COMBS] {
            std::array::from_fn(|i| {
                let len = ((COMB_LENGTHS[i] + offset * 23) as f64 * scale) as usize;
                CombFilter::new(len, decay, damping)
            })
        };

        let make_allpass = |offset: usize| -> [AllpassFilter; NUM_ALLPASS] {
            std::array::from_fn(|i| {
                let len = ((ALLPASS_LENGTHS[i] + offset * 7) as f64 * scale) as usize;
                AllpassFilter::new(len, 0.5)
            })
        };

        let predelay_max = (sample_rate as usize) * 3 / 10; // max 300ms
        let early_max = ((sample_rate as f64 * EARLY_MAX_SECONDS) as usize).max(1);
        Self {
            combs_l: make_combs(0),
            combs_r: make_combs(1),
            allpass_l: make_allpass(0),
            allpass_r: make_allpass(1),
            decay,
            decay_seconds: None,
            damping,
            mix: 0.3,
            modulation: 0.0,
            early_late: 0.0,
            stereo_width: 1.0,
            low_cut_hz: 20.0,
            high_cut_hz: 20_000.0,
            low_cut: [
                BiquadFilter::new(FilterType::HighPass, 20.0, 0.707, 0.0, sample_rate),
                BiquadFilter::new(FilterType::HighPass, 20.0, 0.707, 0.0, sample_rate),
            ],
            high_cut: [
                BiquadFilter::new(FilterType::LowPass, 20_000.0, 0.707, 0.0, sample_rate),
                BiquadFilter::new(FilterType::LowPass, 20_000.0, 0.707, 0.0, sample_rate),
            ],
            lfo_phase: 0.0,
            lfo_offsets: [0.0; NUM_COMBS],
            lfo_countdown: 0,
            predelay_samples: 0,
            predelay_buf_l: vec![0.0; predelay_max],
            predelay_buf_r: vec![0.0; predelay_max],
            predelay_pos: 0,
            early_buf_l: vec![0.0; early_max],
            early_buf_r: vec![0.0; early_max],
            early_pos: 0,
            sample_rate,
        }
    }

    /// Low-frequency T60. For a comb with delay D, g^(T60/D)=0.001.
    /// Damping intentionally shortens high-frequency decay; this is not a
    /// promise of one broadband measured RT60 for the entire reverb network.
    pub fn set_decay_seconds(&mut self, seconds: f64) {
        if !seconds.is_finite() || seconds <= 0.0 {
            return;
        }
        self.decay_seconds = Some(seconds);
        for comb in self.combs_l.iter_mut().chain(self.combs_r.iter_mut()) {
            let delay_seconds = comb.buffer.len() as f64 / self.sample_rate as f64;
            comb.feedback = 10.0_f64.powf(-3.0 * delay_seconds / seconds) as f32;
        }
    }

    fn update_cuts(&mut self) {
        for filter in &mut self.low_cut {
            filter.set_params(self.low_cut_hz, 0.707, 0.0);
        }
        for filter in &mut self.high_cut {
            filter.set_params(self.high_cut_hz, 0.707, 0.0);
        }
    }

    /// Low-frequency T60 the current decay setting produces, in seconds.
    ///
    /// [`Self::set_decay_seconds`] states it directly; the legacy feedback
    /// parameter only fixes the per-comb loop gain, so the time follows from
    /// the longest comb delay with `g^(T60/D) = 0.001`.
    fn tail_seconds(&self) -> f64 {
        if let Some(seconds) = self.decay_seconds {
            return seconds;
        }
        let feedback = self.decay;
        if !(feedback > 0.0 && feedback < 1.0) {
            return 0.0;
        }
        let longest = self
            .combs_l
            .iter()
            .chain(self.combs_r.iter())
            .map(|comb| comb.buffer.len())
            .max()
            .unwrap_or(0);
        let delay_seconds = longest as f64 / f64::from(self.sample_rate.max(1));
        delay_seconds * 0.001_f64.ln() / feedback.ln()
    }
}

fn early_taps(buffer: &[f32], position: usize, taps: &[(f64, f32); 4], sample_rate: f64) -> f32 {
    let len = buffer.len();
    let mut sum = 0.0f32;
    for (seconds, gain) in taps {
        let delay = (seconds * sample_rate) as usize;
        let read = if position >= delay {
            position - delay
        } else {
            position + len - delay
        };
        sum += buffer[read] * gain;
    }
    sum
}

impl AudioEffect for Reverb {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let len = buffer.len();
        let wet = self.mix;
        let dry = 1.0 - self.mix;
        let early_active = self.early_late > 0.0;
        let late_gain = (1.0 - self.early_late) as f32;
        let early_gain = self.early_late as f32;
        let modulation_on = self.modulation > 0.0;
        let modulation_depth = self.modulation * MODULATION_MAX_SECONDS * self.sample_rate as f64;
        let low_cut_active = self.low_cut_hz > 20.0;
        let high_cut_active = self.high_cut_hz < 20_000.0;
        let width = self.stereo_width as f32;
        let mut i = 0;

        while i + 1 < len {
            let in_l = buffer[i];
            let in_r = buffer[i + 1];

            // Predelay
            let pd_l;
            let pd_r;
            if self.predelay_samples > 0 {
                let pd_len = self.predelay_buf_l.len();
                let read_pos = (self.predelay_pos + pd_len - self.predelay_samples) % pd_len;
                pd_l = self.predelay_buf_l[read_pos];
                pd_r = self.predelay_buf_r[read_pos];
                self.predelay_buf_l[self.predelay_pos] = in_l;
                self.predelay_buf_r[self.predelay_pos] = in_r;
                self.predelay_pos = (self.predelay_pos + 1) % pd_len;
            } else {
                pd_l = in_l;
                pd_r = in_r;
            }

            let mut early_l = 0.0f32;
            let mut early_r = 0.0f32;
            if early_active {
                let early_len = self.early_buf_l.len();
                self.early_buf_l[self.early_pos] = in_l;
                self.early_buf_r[self.early_pos] = in_r;
                early_l = early_taps(
                    &self.early_buf_l,
                    self.early_pos,
                    &EARLY_LEFT,
                    self.sample_rate as f64,
                );
                early_r = early_taps(
                    &self.early_buf_r,
                    self.early_pos,
                    &EARLY_RIGHT,
                    self.sample_rate as f64,
                );
                self.early_pos = (self.early_pos + 1) % early_len;
            }

            let mut offsets = [0.0f64; NUM_COMBS];
            if modulation_on {
                if self.lfo_countdown == 0 {
                    self.lfo_phase += std::f64::consts::TAU * MODULATION_RATE_HZ
                        / self.sample_rate as f64
                        * MODULATION_CONTROL_SAMPLES as f64;
                    if self.lfo_phase > std::f64::consts::TAU {
                        self.lfo_phase -= std::f64::consts::TAU;
                    }
                    for (index, offset) in self.lfo_offsets.iter_mut().enumerate() {
                        *offset = modulation_depth * (self.lfo_phase + index as f64 * 0.8).sin();
                    }
                    self.lfo_countdown = MODULATION_CONTROL_SAMPLES;
                } else {
                    self.lfo_countdown -= 1;
                }
                offsets = self.lfo_offsets;
            }

            // Parallel comb filters
            let mut wet_l = 0.0f32;
            let mut wet_r = 0.0f32;
            if modulation_on {
                for (index, comb) in self.combs_l.iter_mut().enumerate() {
                    wet_l += comb.process_modulated(pd_l, offsets[index]);
                }
                for (index, comb) in self.combs_r.iter_mut().enumerate() {
                    wet_r += comb.process_modulated(pd_r, offsets[index]);
                }
            } else {
                for comb in &mut self.combs_l {
                    wet_l += comb.process(pd_l);
                }
                for comb in &mut self.combs_r {
                    wet_r += comb.process(pd_r);
                }
            }
            wet_l /= NUM_COMBS as f32;
            wet_r /= NUM_COMBS as f32;

            // Series allpass filters
            for ap in &mut self.allpass_l {
                wet_l = ap.process(wet_l);
            }
            for ap in &mut self.allpass_r {
                wet_r = ap.process(wet_r);
            }

            let (mut out_l, mut out_r) = if early_active {
                (
                    wet_l * late_gain + early_l * early_gain,
                    wet_r * late_gain + early_r * early_gain,
                )
            } else {
                (wet_l, wet_r)
            };

            if low_cut_active {
                out_l = self.low_cut[0].process_sample(0, out_l as f64) as f32;
                out_r = self.low_cut[1].process_sample(1, out_r as f64) as f32;
            }
            if high_cut_active {
                out_l = self.high_cut[0].process_sample(0, out_l as f64) as f32;
                out_r = self.high_cut[1].process_sample(1, out_r as f64) as f32;
            }
            if self.stereo_width != 1.0 {
                let mid = (out_l + out_r) * 0.5;
                let side = (out_l - out_r) * 0.5 * width;
                out_l = mid + side;
                out_r = mid - side;
            }

            buffer[i] = in_l * dry as f32 + out_l * wet as f32;
            buffer[i + 1] = in_r * dry as f32 + out_r * wet as f32;
            i += 2;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        match name {
            "decay" => {
                self.decay_seconds = None;
                self.decay = value.clamp(0.1, 0.99);
                for c in &mut self.combs_l {
                    c.feedback = self.decay as f32;
                }
                for c in &mut self.combs_r {
                    c.feedback = self.decay as f32;
                }
            }
            "damping" => {
                self.damping = value.clamp(0.0, 1.0);
                for c in &mut self.combs_l {
                    c.damping = self.damping as f32;
                }
                for c in &mut self.combs_r {
                    c.damping = self.damping as f32;
                }
            }
            "mix" => self.mix = value.clamp(0.0, 1.0),
            "predelay" => {
                let ms = value.clamp(0.0, 300.0);
                self.predelay_samples = (ms * 0.001 * self.sample_rate as f64) as usize;
            }
            "modulation" => self.modulation = (value * 0.01).clamp(0.0, 1.0),
            "early_late" => self.early_late = (value * 0.01).clamp(0.0, 1.0),
            "stereo_width" => self.stereo_width = (value * 0.01).clamp(0.0, 2.0),
            "low_cut" => {
                self.low_cut_hz = value.clamp(20.0, 2_000.0);
                self.update_cuts();
            }
            "high_cut" => {
                self.high_cut_hz = value.clamp(2_000.0, 20_000.0);
                self.update_cuts();
            }
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "decay" => Some(self.decay),
            "decay_seconds" => self.decay_seconds,
            "predelay" => Some(self.predelay_samples as f64 / self.sample_rate as f64 * 1000.0),
            "damping" => Some(self.damping),
            "mix" => Some(self.mix),
            "modulation" => Some(self.modulation * 100.0),
            "early_late" => Some(self.early_late * 100.0),
            "stereo_width" => Some(self.stereo_width * 100.0),
            "low_cut" => Some(self.low_cut_hz),
            "high_cut" => Some(self.high_cut_hz),
            _ => None,
        }
    }

    fn reset(&mut self) {
        for c in &mut self.combs_l {
            c.reset();
        }
        for c in &mut self.combs_r {
            c.reset();
        }
        for a in &mut self.allpass_l {
            a.reset();
        }
        for a in &mut self.allpass_r {
            a.reset();
        }
        self.predelay_buf_l.fill(0.0);
        self.predelay_buf_r.fill(0.0);
        self.predelay_pos = 0;
        self.early_buf_l.fill(0.0);
        self.early_buf_r.fill(0.0);
        self.early_pos = 0;
        self.lfo_phase = 0.0;
        self.lfo_offsets = [0.0; NUM_COMBS];
        self.lfo_countdown = 0;
        for filter in &mut self.low_cut {
            filter.reset();
        }
        for filter in &mut self.high_cut {
            filter.reset();
        }
    }

    fn name(&self) -> &str {
        "Reverb"
    }

    fn bounce_tail_samples(&self) -> u32 {
        // A fully dry reverb passes no wet signal, so a bounce keeps nothing.
        if self.mix <= 0.0 {
            return 0;
        }
        let rate = f64::from(self.sample_rate.max(1));
        let seconds = self.tail_seconds() + self.predelay_samples as f64 / rate;
        (seconds * rate).round().clamp(0.0, f64::from(u32::MAX)) as u32
    }
}

// ============================================================================
// Comb filter with damping (lowpass in feedback loop)
// ============================================================================

struct CombFilter {
    buffer: Vec<f32>,
    pos: usize,
    feedback: f32,
    damping: f32,
    damp_state: f32,
}

impl CombFilter {
    fn new(length: usize, feedback: f64, damping: f64) -> Self {
        Self {
            buffer: vec![0.0; length.max(1)],
            pos: 0,
            feedback: feedback as f32,
            damping: damping as f32,
            damp_state: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.buffer[self.pos];

        // One-pole lowpass in feedback
        self.damp_state = output * (1.0 - self.damping) + self.damp_state * self.damping;

        self.buffer[self.pos] = input + self.damp_state * self.feedback;
        self.pos = (self.pos + 1) % self.buffer.len();
        output
    }

    fn process_modulated(&mut self, input: f32, offset: f64) -> f32 {
        let len = self.buffer.len();
        let mut read = self.pos as f64 - offset;
        if read < 0.0 {
            read += len as f64;
        } else if read >= len as f64 {
            read -= len as f64;
        }
        let index = read.floor() as usize;
        let fraction = read - index as f64;
        let next = (index + 1) % len;
        let output = (self.buffer[index] as f64 * (1.0 - fraction)
            + self.buffer[next] as f64 * fraction) as f32;

        // One-pole lowpass in feedback
        self.damp_state = output * (1.0 - self.damping) + self.damp_state * self.damping;

        self.buffer[self.pos] = input + self.damp_state * self.feedback;
        self.pos = (self.pos + 1) % len;
        output
    }

    fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.damp_state = 0.0;
    }
}

// ============================================================================
// Allpass filter
// ============================================================================

struct AllpassFilter {
    buffer: Vec<f32>,
    pos: usize,
    feedback: f32,
}

impl AllpassFilter {
    fn new(length: usize, feedback: f64) -> Self {
        Self {
            buffer: vec![0.0; length.max(1)],
            pos: 0,
            feedback: feedback as f32,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.buffer[self.pos];
        let output = -input + delayed;
        self.buffer[self.pos] = input + delayed * self.feedback;
        self.pos = (self.pos + 1) % self.buffer.len();
        output
    }

    fn reset(&mut self) {
        self.buffer.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(frames: usize) -> Vec<f32> {
        let mut buffer = vec![0.0f32; frames * 2];
        let mut state = 0x1234_5678u32;
        for sample in &mut buffer {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *sample = (state >> 8) as f32 / 8_388_608.0 - 1.0;
        }
        buffer
    }

    fn render(reverb: &mut Reverb, frames: usize) -> Vec<f32> {
        let mut buffer = noise(frames);
        reverb.process(&mut buffer, 48_000);
        buffer
    }

    fn hash(buffer: &[f32]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for sample in buffer {
            hash ^= sample.to_bits() as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        hash
    }

    fn legacy_reverb() -> Reverb {
        let mut reverb = Reverb::new(48_000);
        reverb.set_decay_seconds(2.4);
        reverb.set_param("predelay", 20.0);
        reverb.set_param("damping", 30.0);
        reverb.set_param("mix", 30.0);
        reverb
    }

    #[test]
    fn legacy_parameters_keep_the_frozen_render() {
        let output = render(&mut legacy_reverb(), 96_000);
        assert_eq!(hash(&output), 0x0fa6_c355_199c_dd3c);
    }

    #[test]
    fn modulation_changes_the_tail_only_when_enabled() {
        let reference = render(&mut legacy_reverb(), 24_000);
        let mut modulated = legacy_reverb();
        modulated.set_param("modulation", 60.0);
        let changed = render(&mut modulated, 24_000);
        assert_ne!(reference, changed);
        let mut off = legacy_reverb();
        off.set_param("modulation", 0.0);
        assert_eq!(reference, render(&mut off, 24_000));
    }

    #[test]
    fn early_late_and_cuts_change_the_wet_signal() {
        for (name, value) in [
            ("early_late", 50.0),
            ("stereo_width", 140.0),
            ("low_cut", 400.0),
            ("high_cut", 4_000.0),
        ] {
            let reference = render(&mut legacy_reverb(), 24_000);
            let mut reverb = legacy_reverb();
            reverb.set_param(name, value);
            let changed = render(&mut reverb, 24_000);
            assert_ne!(reference, changed, "{name} did not change the tail");
        }
    }

    #[test]
    fn a_long_predelay_delays_the_wet_signal_by_the_applied_value() {
        let rate = 48_000;
        let mut reverb = Reverb::new(rate);
        reverb.set_param("mix", 100.0);
        reverb.set_param("predelay", 250.0);
        // The readback is the applied value, not the request.
        assert_eq!(reverb.get_param("predelay"), Some(250.0));

        let frames = 16_384; // 341 ms, past the 250 ms the wet path waits
        let mut buffer = vec![0.0f32; frames * 2];
        buffer[0] = 1.0;
        buffer[1] = 1.0;
        reverb.process(&mut buffer, rate);
        let first = buffer
            .chunks_exact(2)
            .position(|frame| frame[0].abs() > 1e-6 || frame[1].abs() > 1e-6)
            .expect("the wet signal arrives");
        assert!(
            first >= 12_000,
            "the wet path started at {first} frames, before the 250 ms pre-delay"
        );
    }

    #[test]
    fn a_predelay_longer_than_the_range_reports_the_clamped_value() {
        let mut reverb = Reverb::new(48_000);
        reverb.set_param("predelay", 400.0);
        assert_eq!(reverb.get_param("predelay"), Some(300.0));
    }

    #[test]
    fn width_one_hundred_is_the_bit_exact_identity() {
        let reference = render(&mut legacy_reverb(), 24_000);
        let mut reverb = legacy_reverb();
        reverb.set_param("stereo_width", 100.0);
        assert_eq!(reference, render(&mut reverb, 24_000));
    }

    #[test]
    fn extreme_settings_stay_finite() {
        let mut reverb = Reverb::new(48_000);
        reverb.set_decay_seconds(60.0);
        reverb.set_param("predelay", 100.0);
        reverb.set_param("damping", 0.0);
        reverb.set_param("modulation", 100.0);
        reverb.set_param("early_late", 100.0);
        reverb.set_param("stereo_width", 200.0);
        reverb.set_param("low_cut", 1_000.0);
        reverb.set_param("high_cut", 2_000.0);
        reverb.set_param("mix", 100.0);
        let output = render(&mut reverb, 48_000);
        assert!(output.iter().all(|value| value.is_finite()));
        assert!(output.iter().all(|value| value.abs() < 1_000.0));
    }

    #[test]
    fn reset_is_deterministic_with_every_control_active() {
        let mut reverb = Reverb::new(48_000);
        reverb.set_param("modulation", 70.0);
        reverb.set_param("early_late", 40.0);
        reverb.set_param("low_cut", 80.0);
        reverb.set_param("high_cut", 9_000.0);
        reverb.set_param("stereo_width", 130.0);
        let first = render(&mut reverb, 24_000);
        reverb.reset();
        let second = render(&mut reverb, 24_000);
        assert_eq!(first, second);
    }

    #[test]
    fn bounce_tail_follows_the_decay_and_never_moves_the_full_mix_length() {
        let rate = 48_000;
        let mut reverb = Reverb::new(rate);
        reverb.set_param("mix", 0.3);
        reverb.set_decay_seconds(2.0);
        assert_eq!(reverb.tail_samples(), 0, "the full-mix length is unchanged");
        assert_eq!(reverb.bounce_tail_samples(), 96_000);

        reverb.set_decay_seconds(0.5);
        assert_eq!(
            reverb.bounce_tail_samples(),
            24_000,
            "a short decay rings shorter"
        );

        reverb.set_param("mix", 0.0);
        assert_eq!(
            reverb.bounce_tail_samples(),
            0,
            "a dry reverb keeps nothing"
        );
    }

    #[test]
    fn bounce_tail_covers_the_predelay_and_the_legacy_feedback_decay() {
        let rate = 48_000;
        let mut reverb = Reverb::new(rate);
        reverb.set_param("mix", 0.3);
        reverb.set_param("predelay", 50.0);
        reverb.set_decay_seconds(1.0);
        assert_eq!(reverb.bounce_tail_samples(), 50_400, "1 s plus 50 ms");

        // The legacy feedback value is a loop gain, not a time; the tail
        // follows from the longest comb delay.
        reverb.set_param("decay", 0.8);
        let seconds = f64::from(reverb.bounce_tail_samples()) / f64::from(rate) - 0.05;
        assert!(
            (0.5..2.0).contains(&seconds),
            "legacy decay 0.8 rings for {seconds}s"
        );
    }
}
