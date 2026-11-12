//! True-peak mastering limiter with polyphase oversampling and lookahead.
//!
//! Parameter ids (unit, range, default):
//! - `ceiling` (dBTP with `true_peak=1`, otherwise dBFS, -12..0, -0.3)
//! - `release` (ms, 5..500, 50)
//! - `release_mode` (0 = fast, 1 = auto, 2 = punch, 3 = sustain, 1)
//! - `lookahead` (ms, 0..20, 5)
//! - `true_peak` (0/1, 1)
//! - `oversampling` (0 = 4x, 1 = 8x, 2 = 16x, 0)
//! - `link` (percent, 0..100, 100)
//!
//! `release_mode=fast` and `true_peak=0` are the explicit legacy path: they
//! reproduce the pre-mastering sample-peak limiter when the lookahead is 1 ms.
//!
//! Detection uses a polyphase windowed-sinc interpolator (32 taps per phase,
//! Blackman-Harris window) over the same 32-frame support at every factor, so
//! the detector group delay stays ~16 frames; only the phase grid gets finer.
//! The ceiling is compared against the inter-sample peak. The 5 ms default
//! lookahead leaves about 224 frames (4.7 ms) of opening. Nothing here
//! allocates, locks or starts a thread after `new`.

use super::AudioEffect;
use std::collections::VecDeque;

const MAX_LOOKAHEAD_MS: f64 = 20.0;
/// Default polyphase count for the true-peak detector.
pub const TRUE_PEAK_PHASES: usize = 4;
/// FIR taps per true-peak phase.
pub const TRUE_PEAK_TAPS: usize = 32;
/// Largest polyphase count the detector accepts.
pub const TRUE_PEAK_MAX_PHASES: usize = 16;
/// Selectable detector factors; index 0 is the original 4x path.
pub const OVERSAMPLING_FACTORS: [usize; 3] = [4, 8, 16];
/// Extra headroom so the 4x estimate cannot leave an intersample overshoot.
const TP_SAFETY: f64 = 0.995;
/// Auto release reaches `AUTO_RELEASE_SCALE` times the knob at full depth.
const AUTO_RELEASE_SCALE: f64 = 3.0;
/// Punch release depends on the current gain reduction only.
const PUNCH_RELEASE_SCALE: f64 = 1.2;
/// Sustain release depends on the programme-duration memory only.
const SUSTAIN_RELEASE_SCALE: f64 = 5.0;
/// Gain reduction treated as full program depth for the auto release.
const AUTO_DEPTH_DB: f64 = 12.0;
/// Time constants for the auto release's sustained-limiting memory.
const AUTO_SUSTAIN_RISE_S: f64 = 2.0;
const AUTO_SUSTAIN_FALL_S: f64 = 3.0;
/// Decay of the peak program-depth memory that shapes the release tail.
const AUTO_MEMORY_FALL_S: f64 = 3.0;
/// 0.5 dB buckets from 0 to 24 dB.
const GR_HISTOGRAM_BUCKETS: usize = 49;

/// Windowed-sinc interpolation coefficients, shared with offline analysis.
pub fn true_peak_coefficients() -> Vec<f64> {
    true_peak_coefficients_for(TRUE_PEAK_PHASES)
}

/// Coefficients for `phases` interpolated points per input frame.
pub fn true_peak_coefficients_for(phases: usize) -> Vec<f64> {
    let phases = phases.clamp(1, TRUE_PEAK_MAX_PHASES);
    let length = phases * TRUE_PEAK_TAPS;
    let center = (length as f64 - 1.0) / 2.0;
    let mut coeffs = vec![0.0_f64; length];
    for (index, coefficient) in coeffs.iter_mut().enumerate() {
        let x = (index as f64 - center) / phases as f64;
        let sinc = if x.abs() < 1e-9 {
            1.0
        } else {
            let pix = std::f64::consts::PI * x;
            pix.sin() / pix
        };
        let phase = 2.0 * std::f64::consts::PI * index as f64 / (length - 1) as f64;
        let window = 0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos();
        *coefficient = sinc * window;
    }
    coeffs
}

/// Polyphase true-peak detector over interleaved stereo frames.
pub struct TruePeakDetector {
    phases: usize,
    coeffs: Vec<f64>,
    history: [[f64; TRUE_PEAK_TAPS]; 2],
    position: usize,
}

impl Default for TruePeakDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl TruePeakDetector {
    /// A detector with the default phase count.
    pub fn new() -> Self {
        Self::with_phases(TRUE_PEAK_PHASES)
    }

    /// A detector with the phase count clamped to the supported range.
    pub fn with_phases(phases: usize) -> Self {
        let phases = phases.clamp(1, TRUE_PEAK_MAX_PHASES);
        Self {
            phases,
            coeffs: true_peak_coefficients_for(phases),
            history: [[0.0; TRUE_PEAK_TAPS]; 2],
            position: 0,
        }
    }

    /// Peak of each channel across every interpolated phase this frame.
    pub fn peak_stereo(&mut self, left: f32, right: f32) -> (f64, f64) {
        self.history[0][self.position] = f64::from(left);
        self.history[1][self.position] = f64::from(right);
        let mut peak_l = f64::from(left.abs());
        let mut peak_r = f64::from(right.abs());
        for phase in 0..self.phases {
            let (mut left_sum, mut right_sum) = (0.0_f64, 0.0_f64);
            for tap in 0..TRUE_PEAK_TAPS {
                let index = (self.position + TRUE_PEAK_TAPS - tap) % TRUE_PEAK_TAPS;
                let coefficient = self.coeffs[tap * self.phases + phase];
                left_sum += self.history[0][index] * coefficient;
                right_sum += self.history[1][index] * coefficient;
            }
            peak_l = peak_l.max(left_sum.abs());
            peak_r = peak_r.max(right_sum.abs());
        }
        self.position = (self.position + 1) % TRUE_PEAK_TAPS;
        (peak_l, peak_r)
    }

    /// Peak across the two channels and every interpolated phase this frame.
    pub fn peak(&mut self, left: f32, right: f32) -> f64 {
        let (peak_l, peak_r) = self.peak_stereo(left, right);
        peak_l.max(peak_r)
    }

    /// Configured polyphase count.
    pub fn phases(&self) -> usize {
        self.phases
    }

    /// Clear the sample history and phase position.
    pub fn reset(&mut self) {
        self.history = [[0.0; TRUE_PEAK_TAPS]; 2];
        self.position = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Release law used by the limiter envelope.
pub enum ReleaseMode {
    Fast,
    Auto,
    Punch,
    Sustain,
}

impl ReleaseMode {
    fn from_value(value: f64) -> Self {
        match value.round() as i32 {
            0 => Self::Fast,
            2 => Self::Punch,
            3 => Self::Sustain,
            _ => Self::Auto,
        }
    }

    fn value(self) -> f64 {
        match self {
            Self::Fast => 0.0,
            Self::Auto => 1.0,
            Self::Punch => 2.0,
            Self::Sustain => 3.0,
        }
    }
}

/// Release law settings shared by every detector chain.
#[derive(Clone, Copy)]
struct Release {
    mode: ReleaseMode,
    release_ms: f64,
    fast_coeff: f64,
    sample_rate: f64,
}

/// One detector chain: the lookahead peak window and the release programme
/// memory of one signal, either the linked sum or a single channel.
struct Detector {
    window_peaks: VecDeque<(u64, f64)>,
    frame_index: u64,
    envelope: f64,
    program_depth: f64,
    program_memory: f64,
    program_sustain: f64,
}

impl Detector {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            window_peaks: VecDeque::with_capacity(capacity),
            frame_index: 0,
            envelope: 0.0,
            program_depth: 0.0,
            program_memory: 0.0,
            program_sustain: 0.0,
        }
    }

    fn clear(&mut self) {
        self.window_peaks.clear();
        self.frame_index = 0;
        self.envelope = 0.0;
        self.program_depth = 0.0;
        self.program_memory = 0.0;
        self.program_sustain = 0.0;
    }

    fn update_program(&mut self, peak: f64, ceiling_lin: f64, sample_rate: f64) {
        let frames = sample_rate;
        if peak > ceiling_lin {
            let rise = 1.0 - (-1.0 / (AUTO_SUSTAIN_RISE_S * frames)).exp();
            self.program_sustain += (1.0 - self.program_sustain) * rise;
        } else {
            let fall = 1.0 - (-1.0 / (AUTO_SUSTAIN_FALL_S * frames)).exp();
            self.program_sustain -= self.program_sustain * fall;
        }
        let depth_db = if self.envelope > ceiling_lin {
            linear_to_db(self.envelope) - linear_to_db(ceiling_lin)
        } else {
            0.0
        };
        self.program_depth = (depth_db / AUTO_DEPTH_DB).clamp(0.0, 1.0);
        self.program_memory = if self.program_depth > self.program_memory {
            self.program_depth
        } else {
            let decay = (-1.0 / (AUTO_MEMORY_FALL_S * frames)).exp();
            self.program_memory * decay
        };
    }

    fn release_value(&self, release: Release) -> f64 {
        let tau_ms = match release.mode {
            ReleaseMode::Fast => return release.fast_coeff,
            ReleaseMode::Auto => {
                let program = self
                    .program_depth
                    .max(self.program_memory)
                    .max(self.program_sustain);
                release.release_ms * (1.0 + AUTO_RELEASE_SCALE * program)
            }
            ReleaseMode::Punch => {
                release.release_ms * (1.0 + PUNCH_RELEASE_SCALE * self.program_depth)
            }
            ReleaseMode::Sustain => {
                release.release_ms * (1.0 + SUSTAIN_RELEASE_SCALE * self.program_sustain)
            }
        };
        (-1.0 / (tau_ms * 0.001 * release.sample_rate)).exp()
    }

    /// Peak held by the lookahead window and the gain it requires.
    fn gain(
        &mut self,
        incoming: f64,
        ceiling_lin: f64,
        lookahead: usize,
        release: Release,
    ) -> (f64, f64) {
        // Hold each peak until its delayed sample has actually left the
        // lookahead buffer. Releasing earlier lets an isolated transient
        // overshoot the declared ceiling at the output.
        let oldest = self.frame_index.saturating_sub(lookahead as u64);
        while self
            .window_peaks
            .front()
            .is_some_and(|(frame, _)| *frame < oldest)
        {
            self.window_peaks.pop_front();
        }
        while self
            .window_peaks
            .back()
            .is_some_and(|(_, peak)| *peak <= incoming)
        {
            self.window_peaks.pop_back();
        }
        self.window_peaks.push_back((self.frame_index, incoming));
        self.frame_index += 1;
        let peak = self.window_peaks.front().unwrap().1;

        // Envelope: instant attack, fast or program-dependent release.
        if peak > self.envelope {
            self.envelope = peak;
        } else {
            self.update_program(peak, ceiling_lin, release.sample_rate);
            let coeff = self.release_value(release);
            self.envelope = coeff * self.envelope + (1.0 - coeff) * peak;
        }

        let gain = if self.envelope > ceiling_lin {
            ceiling_lin / self.envelope
        } else {
            1.0
        };
        (peak, gain)
    }
}

/// Summary of the gain-reduction distribution over all processed frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainReductionSummary {
    pub max_db: f64,
    pub mean_db: f64,
    /// Percentiles over every frame, including untouched frames.
    pub p50_db: f64,
    pub p95_db: f64,
    pub p99_db: f64,
    pub limited_fraction: f64,
}

/// A lookahead limiter with linked or per-channel detection.
pub struct Limiter {
    ceiling_db: f64,
    ceiling_lin: f64,
    release_ms: f64,
    release_coeff: f64,
    release_mode: ReleaseMode,
    lookahead_ms: f64,
    // Lookahead ring (max length, current delay selected by read offset)
    lookahead_buf_l: Vec<f32>,
    lookahead_buf_r: Vec<f32>,
    lookahead_pos: usize,
    lookahead_len: usize,
    true_peak: bool,
    oversampling: usize,
    oversampler: TruePeakDetector,
    /// Channel link in percent: 100 uses the linked detector, 0 each channel.
    link_percent: f64,
    /// Detector chain over the linked stereo peak.
    linked: Detector,
    /// Detector chains for the left and right channels.
    channels: [Detector; 2],
    last_gr_db: f64,
    max_gr_db: f64,
    gr_sum_db: f64,
    gr_frames: u64,
    gr_limited_frames: u64,
    gr_histogram: [u64; GR_HISTOGRAM_BUCKETS],
    /// Detector peak of the current frame and the maximum since reset.
    last_input_peak_db: f64,
    max_input_peak_db: f64,
    sample_rate: u32,
}

impl Limiter {
    /// Prepare a limiter with the default ceiling, release and lookahead.
    pub fn new(sample_rate: u32) -> Self {
        let max_lookahead = ((sample_rate as f64 * MAX_LOOKAHEAD_MS * 0.001) as usize).max(1);
        let default_lookahead = ((sample_rate as f64 * 5.0 * 0.001) as usize).max(1);
        let mut s = Self {
            ceiling_db: -0.3,
            ceiling_lin: db_to_lin(-0.3),
            release_ms: 50.0,
            release_coeff: 0.0,
            release_mode: ReleaseMode::Auto,
            lookahead_ms: 5.0,
            lookahead_buf_l: vec![0.0; max_lookahead + 1],
            lookahead_buf_r: vec![0.0; max_lookahead + 1],
            lookahead_pos: 0,
            lookahead_len: default_lookahead.min(max_lookahead),
            true_peak: true,
            oversampling: TRUE_PEAK_PHASES,
            oversampler: TruePeakDetector::new(),
            link_percent: 100.0,
            linked: Detector::with_capacity(max_lookahead + 1),
            channels: [
                Detector::with_capacity(max_lookahead + 1),
                Detector::with_capacity(max_lookahead + 1),
            ],
            last_gr_db: 0.0,
            max_gr_db: 0.0,
            gr_sum_db: 0.0,
            gr_frames: 0,
            gr_limited_frames: 0,
            gr_histogram: [0; GR_HISTOGRAM_BUCKETS],
            last_input_peak_db: f64::NEG_INFINITY,
            max_input_peak_db: f64::NEG_INFINITY,
            sample_rate,
        };
        s.update_coeff();
        s
    }

    fn update_coeff(&mut self) {
        self.release_coeff = (-1.0 / (self.release_ms * 0.001 * self.sample_rate as f64)).exp();
    }

    fn update_lookahead(&mut self) {
        let max_frames = self.lookahead_buf_l.len() - 1;
        let requested = (self.sample_rate as f64 * self.lookahead_ms * 0.001) as usize;
        self.lookahead_len = if self.lookahead_ms > 0.0 {
            requested.clamp(1, max_frames)
        } else {
            0
        };
        self.lookahead_buf_l.fill(0.0);
        self.lookahead_buf_r.fill(0.0);
        self.lookahead_pos = 0;
        self.linked.window_peaks.clear();
        for detector in &mut self.channels {
            detector.window_peaks.clear();
        }
    }

    fn record_gain_reduction(&mut self, gr_db: f64) {
        self.last_gr_db = gr_db;
        self.max_gr_db = self.max_gr_db.max(gr_db);
        self.gr_sum_db += gr_db;
        self.gr_frames += 1;
        if gr_db > 0.0 {
            self.gr_limited_frames += 1;
        }
        let bucket = ((gr_db.max(0.0) / 0.5) as usize).min(GR_HISTOGRAM_BUCKETS - 1);
        self.gr_histogram[bucket] += 1;
    }

    /// Peak and mean gain reduction in dB over all frames since construction or
    /// `reset`, including frames where the limiter passed audio untouched.
    pub fn gain_reduction_stats(&self) -> (f64, f64) {
        let mean = if self.gr_frames == 0 {
            0.0
        } else {
            self.gr_sum_db / self.gr_frames as f64
        };
        (self.max_gr_db, mean)
    }

    /// Current-frame detector peak in dBFS (inter-sample when true peak is on).
    pub fn input_peak_dbfs(&self) -> f64 {
        self.last_input_peak_db
    }

    /// Maximum detector peak seen since construction or `reset`, in dBFS.
    pub fn max_input_peak_dbfs(&self) -> f64 {
        self.max_input_peak_db
    }

    /// Headroom to the ceiling in dB: positive below, negative above.
    pub fn ceiling_margin_db(&self) -> f64 {
        self.ceiling_db - self.last_input_peak_db
    }

    /// Percentile summary of gain reduction since construction or `reset`.
    pub fn gain_reduction_summary(&self) -> GainReductionSummary {
        let total = self.gr_frames.max(1) as f64;
        let percentile = |fraction: f64| {
            let target = (fraction * self.gr_frames as f64).ceil().max(1.0) as u64;
            let mut cumulative = 0_u64;
            for (bucket, &count) in self.gr_histogram.iter().enumerate() {
                cumulative += count;
                if cumulative >= target {
                    return bucket as f64 * 0.5;
                }
            }
            (GR_HISTOGRAM_BUCKETS - 1) as f64 * 0.5
        };
        GainReductionSummary {
            max_db: self.max_gr_db,
            mean_db: if self.gr_frames == 0 {
                0.0
            } else {
                self.gr_sum_db / total
            },
            p50_db: percentile(0.50),
            p95_db: percentile(0.95),
            p99_db: percentile(0.99),
            limited_fraction: self.gr_limited_frames as f64 / total,
        }
    }
}

impl AudioEffect for Limiter {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let len = buffer.len();
        let ceil = if self.true_peak {
            self.ceiling_lin * TP_SAFETY
        } else {
            self.ceiling_lin
        };
        let ring_len = self.lookahead_buf_l.len();
        let lookahead = self.lookahead_len;
        let release = Release {
            mode: self.release_mode,
            release_ms: self.release_ms,
            fast_coeff: self.release_coeff,
            sample_rate: self.sample_rate as f64,
        };
        let split = self.link_percent < 100.0;
        let link = self.link_percent / 100.0;
        let mut i = 0;

        while i + 1 < len {
            let in_l = buffer[i];
            let in_r = buffer[i + 1];

            self.lookahead_buf_l[self.lookahead_pos] = in_l;
            self.lookahead_buf_r[self.lookahead_pos] = in_r;
            let read = (self.lookahead_pos + ring_len - lookahead) % ring_len;
            let out_l = self.lookahead_buf_l[read] as f64;
            let out_r = self.lookahead_buf_r[read] as f64;
            self.lookahead_pos = if self.lookahead_pos + 1 == ring_len {
                0
            } else {
                self.lookahead_pos + 1
            };

            let (incoming_l, incoming_r) = if self.true_peak {
                self.oversampler.peak_stereo(in_l, in_r)
            } else {
                ((in_l as f64).abs(), (in_r as f64).abs())
            };
            let incoming = incoming_l.max(incoming_r);
            let incoming_db = linear_to_db(incoming.max(1e-12));
            self.max_input_peak_db = self.max_input_peak_db.max(incoming_db);

            let (peak, linked_gain) = self.linked.gain(incoming, ceil, lookahead, release);
            self.last_input_peak_db = linear_to_db(peak.max(1e-12));

            // At partial link the applied gain is the amplitude blend of the
            // linked detector and that channel's own detector.
            let (gain_l, gain_r) = if split {
                let (_, own_l) = self.channels[0].gain(incoming_l, ceil, lookahead, release);
                let (_, own_r) = self.channels[1].gain(incoming_r, ceil, lookahead, release);
                (
                    link * linked_gain + (1.0 - link) * own_l,
                    link * linked_gain + (1.0 - link) * own_r,
                )
            } else {
                (linked_gain, linked_gain)
            };
            let applied = gain_l.min(gain_r);
            self.record_gain_reduction(if applied < 1.0 {
                -linear_to_db(applied)
            } else {
                0.0
            });

            buffer[i] = (out_l * gain_l) as f32;
            buffer[i + 1] = (out_r * gain_r) as f32;
            i += 2;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        match name {
            "ceiling" => {
                self.ceiling_db = value.clamp(-12.0, 0.0);
                self.ceiling_lin = db_to_lin(self.ceiling_db);
            }
            "release" => {
                self.release_ms = value.clamp(5.0, 500.0);
                self.update_coeff();
            }
            "release_mode" => {
                self.release_mode = ReleaseMode::from_value(value);
            }
            "lookahead" => {
                self.lookahead_ms = value.clamp(0.0, MAX_LOOKAHEAD_MS);
                self.update_lookahead();
            }
            "true_peak" => {
                let enabled = value >= 0.5;
                if enabled != self.true_peak {
                    self.oversampler.reset();
                }
                self.true_peak = enabled;
            }
            "oversampling" => {
                let index = value.round().clamp(0.0, 2.0) as usize;
                let phases = OVERSAMPLING_FACTORS[index];
                if phases != self.oversampling {
                    self.oversampling = phases;
                    self.oversampler = TruePeakDetector::with_phases(phases);
                }
            }
            "link" => {
                let percent = value.clamp(0.0, 100.0);
                if percent != self.link_percent {
                    self.link_percent = percent;
                    for detector in &mut self.channels {
                        detector.clear();
                    }
                }
            }
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "ceiling" => Some(self.ceiling_db),
            "release" => Some(self.release_ms),
            "release_mode" => Some(self.release_mode.value()),
            "lookahead" => Some(self.lookahead_ms),
            "true_peak" => Some(if self.true_peak { 1.0 } else { 0.0 }),
            "oversampling" => Some(
                OVERSAMPLING_FACTORS
                    .iter()
                    .position(|&factor| factor == self.oversampling)
                    .unwrap_or(0) as f64,
            ),
            "link" => Some(self.link_percent),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.last_gr_db = 0.0;
        self.max_gr_db = 0.0;
        self.gr_sum_db = 0.0;
        self.gr_frames = 0;
        self.gr_limited_frames = 0;
        self.gr_histogram = [0; GR_HISTOGRAM_BUCKETS];
        self.last_input_peak_db = f64::NEG_INFINITY;
        self.max_input_peak_db = f64::NEG_INFINITY;
        self.lookahead_buf_l.fill(0.0);
        self.lookahead_buf_r.fill(0.0);
        self.lookahead_pos = 0;
        self.linked.clear();
        for detector in &mut self.channels {
            detector.clear();
        }
        self.oversampler.reset();
    }

    fn name(&self) -> &str {
        "Limiter"
    }

    fn latency_samples(&self) -> u32 {
        self.lookahead_len as u32
    }

    fn gain_reduction_db(&self) -> f64 {
        self.last_gr_db
    }

    fn max_gain_reduction_db(&self) -> f64 {
        self.max_gr_db
    }

    fn ceiling_margin_db(&self) -> Option<f64> {
        Some(self.ceiling_margin_db())
    }
}

fn db_to_lin(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

fn linear_to_db(lin: f64) -> f64 {
    20.0 * lin.max(1e-12).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent windowed-sinc peak measurement for one channel.
    fn measured_channel_peak(signal: &[f32], channel: usize, phases: usize, taps: usize) -> f64 {
        let length = phases * taps;
        let center = (length as f64 - 1.0) / 2.0;
        let mut coeffs = vec![0.0_f64; length];
        for (index, coefficient) in coeffs.iter_mut().enumerate() {
            let x = (index as f64 - center) / phases as f64;
            let sinc = if x.abs() < 1e-9 {
                1.0
            } else {
                let pix = std::f64::consts::PI * x;
                pix.sin() / pix
            };
            let phase = 2.0 * std::f64::consts::PI * index as f64 / (length - 1) as f64;
            *coefficient = sinc * (0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos());
        }
        let mut history = vec![0.0_f64; taps];
        let mut position = 0;
        let mut peak = 0.0_f64;
        for frame in signal.chunks_exact(2) {
            history[position] = frame[channel] as f64;
            for phase in 0..phases {
                let mut sum = 0.0;
                for tap in 0..taps {
                    let index = (position + taps - tap) % taps;
                    sum += history[index] * coeffs[tap * phases + phase];
                }
                peak = peak.max(sum.abs());
            }
            position = (position + 1) % taps;
        }
        peak
    }

    /// Independent windowed-sinc peak measurement for test assertions.
    fn measured_peak(signal: &[f32], phases: usize, taps: usize) -> f64 {
        measured_channel_peak(signal, 0, phases, taps)
            .max(measured_channel_peak(signal, 1, phases, taps))
    }

    fn hot_sine(frequency: f64, amplitude: f64, phase: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f64 / 48_000.0;
                let sample = (2.0 * std::f64::consts::PI * frequency * t + phase).sin() * amplitude;
                [sample as f32, sample as f32]
            })
            .collect()
    }

    /// Left channel driven 6 dB over a -1 dB ceiling, right channel quiet.
    fn panned_burst(frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f64 / 48_000.0;
                let tone = (2.0 * std::f64::consts::PI * 1_000.0 * t).sin();
                [(tone * 2.0) as f32, (tone * 0.05) as f32]
            })
            .collect()
    }

    fn channel_rms(signal: &[f32], channel: usize, start: usize, end: usize) -> f64 {
        let frames = &signal[start * 2..end * 2];
        let sum: f64 = frames
            .chunks_exact(2)
            .map(|frame| {
                let value = f64::from(frame[channel]);
                value * value
            })
            .sum();
        (sum / (frames.len() / 2) as f64).sqrt()
    }

    /// Attenuation in dB of one channel between two renders of the same input.
    fn channel_gain_reduction_db(input: &[f32], output: &[f32], channel: usize) -> f64 {
        let (start, end) = (12_000, 36_000);
        20.0 * (channel_rms(input, channel, start, end) / channel_rms(output, channel, start, end))
            .log10()
    }

    fn legacy_reference(
        input: &[f32],
        ceiling_lin: f64,
        release_coeff: f64,
        lookahead_len: usize,
    ) -> Vec<f32> {
        let mut out = input.to_vec();
        let mut look = [vec![0.0_f32; lookahead_len], vec![0.0_f32; lookahead_len]];
        let mut position = 0;
        let mut envelope = 0.0_f64;
        let mut peaks: VecDeque<(u64, f64)> = VecDeque::with_capacity(lookahead_len + 1);
        let mut frame_index: u64 = 0;
        let mut i = 0;
        while i + 1 < out.len() {
            let out_l = look[0][position];
            let out_r = look[1][position];
            look[0][position] = out[i];
            look[1][position] = out[i + 1];
            position = (position + 1) % lookahead_len;

            let incoming = (out[i] as f64).abs().max((out[i + 1] as f64).abs());
            let oldest = frame_index.saturating_sub(lookahead_len as u64);
            while peaks.front().is_some_and(|(frame, _)| *frame < oldest) {
                peaks.pop_front();
            }
            while peaks.back().is_some_and(|(_, peak)| *peak <= incoming) {
                peaks.pop_back();
            }
            peaks.push_back((frame_index, incoming));
            frame_index += 1;
            let peak = peaks.front().unwrap().1;
            if peak > envelope {
                envelope = peak;
            } else {
                envelope = release_coeff * envelope + (1.0 - release_coeff) * peak;
            }
            let gain = if envelope > ceiling_lin {
                ceiling_lin / envelope
            } else {
                1.0
            };
            out[i] = (out_l as f64 * gain) as f32;
            out[i + 1] = (out_r as f64 * gain) as f32;
            i += 2;
        }
        out
    }

    #[test]
    fn explicit_legacy_path_matches_the_pre_mastering_limiter_exactly() {
        for rate in [44_100, 48_000, 96_000] {
            let input = hot_sine(997.0, 0.9, 0.0, 4800);
            let mut upgraded = input.clone();
            let mut limiter = Limiter::new(rate);
            limiter.set_param("true_peak", 0.0);
            limiter.set_param("release_mode", 0.0);
            limiter.set_param("lookahead", 1.0);
            limiter.process(&mut upgraded, rate);
            let legacy = legacy_reference(
                &input,
                db_to_lin(-0.3),
                limiter.release_coeff,
                limiter.lookahead_len,
            );
            assert_eq!(upgraded, legacy, "rate {rate}");
            assert_eq!(
                limiter.latency_samples(),
                ((rate as f64 * 0.001) as usize).max(1) as u32
            );
        }
    }

    #[test]
    fn defaults_are_a_mastering_limiter() {
        let limiter = Limiter::new(48_000);
        assert_eq!(limiter.get_param("true_peak"), Some(1.0));
        assert_eq!(limiter.get_param("release_mode"), Some(1.0));
        assert_eq!(limiter.get_param("lookahead"), Some(5.0));
        assert_eq!(limiter.get_param("link"), Some(100.0));
        assert_eq!(limiter.latency_samples(), 240);
    }

    #[test]
    fn delayed_impulse_never_exceeds_declared_ceiling() {
        for rate in [44_100, 48_000, 96_000] {
            for release in [5.0, 50.0, 500.0] {
                for release_mode in [0.0, 1.0] {
                    for true_peak in [0.0, 1.0] {
                        let mut limiter = Limiter::new(rate);
                        limiter.set_param("ceiling", -1.0);
                        limiter.set_param("release", release);
                        limiter.set_param("release_mode", release_mode);
                        limiter.set_param("true_peak", true_peak);
                        let mut signal = vec![0.0; rate as usize / 10 * 2];
                        signal[0] = 4.0;
                        signal[1] = -2.0;
                        limiter.process(&mut signal, rate);
                        let peak = signal.iter().fold(0.0_f32, |p, x| p.max(x.abs()));
                        assert!(
                            peak as f64 <= db_to_lin(-1.0) + 1e-6,
                            "ceiling overshoot: rate={rate}, release={release}, mod={release_mode}, tp={true_peak}, peak={peak}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn true_peak_ceiling_holds_on_an_intersample_peak_signal() {
        let mut limiter = Limiter::new(48_000);
        limiter.set_param("ceiling", -1.0);
        limiter.set_param("lookahead", 5.0);
        limiter.set_param("release", 100.0);
        limiter.set_param("release_mode", 1.0);
        limiter.set_param("true_peak", 1.0);
        // 12 kHz at 45 degrees has 0.707 sample peaks but a 1.0 true peak.
        let mut signal = hot_sine(12_000.0, 1.0, std::f64::consts::FRAC_PI_4, 24_000);
        limiter.process(&mut signal, 48_000);
        let sample_peak = signal.iter().fold(0.0_f32, |p, x| p.max(x.abs()));
        let true_peak = measured_peak(&signal, 4, 32);
        assert!(
            true_peak <= db_to_lin(-1.0) + 1e-3,
            "4x true peak {true_peak} exceeds ceiling"
        );
        assert!(sample_peak as f64 <= true_peak + 1e-6);
    }

    #[test]
    fn true_peak_detection_limits_intersample_overshoot_that_sample_peak_ignores() {
        let input = hot_sine(12_000.0, 1.0, std::f64::consts::FRAC_PI_4, 24_000);
        let mut sample_peak = input.clone();
        let mut limiter = Limiter::new(48_000);
        limiter.set_param("ceiling", -1.0);
        limiter.set_param("true_peak", 0.0);
        limiter.process(&mut sample_peak, 48_000);
        let sample_peak_overshoot = measured_peak(&sample_peak, 4, 32);
        assert!(
            sample_peak_overshoot > db_to_lin(-1.0),
            "sample-peak mode must leave the intersample overshoot audible"
        );

        let mut true_peak = input;
        let mut limiter = Limiter::new(48_000);
        limiter.set_param("ceiling", -1.0);
        limiter.set_param("true_peak", 1.0);
        limiter.process(&mut true_peak, 48_000);
        assert!(measured_peak(&true_peak, 4, 32) <= db_to_lin(-1.0) + 1e-3);
    }

    #[test]
    fn ceiling_accuracy_is_within_a_tenth_of_a_db() {
        for release_mode in [0.0, 1.0] {
            let mut limiter = Limiter::new(48_000);
            limiter.set_param("ceiling", -1.0);
            limiter.set_param("true_peak", 1.0);
            limiter.set_param("lookahead", 5.0);
            limiter.set_param("release_mode", release_mode);
            let mut signal = hot_sine(1_000.0, 1.0, 0.0, 48_000);
            limiter.process(&mut signal, 48_000);
            let peak_db = linear_to_db(measured_peak(&signal, 8, 32));
            assert!(
                (peak_db - -1.0).abs() <= 0.1,
                "release mode {release_mode}: ceiling accuracy {peak_db} dB"
            );
        }
    }

    #[test]
    fn gain_reduction_readout_matches_applied_attenuation() {
        let mut limiter = Limiter::new(48_000);
        limiter.set_param("ceiling", -6.0);
        limiter.set_param("true_peak", 1.0);
        let mut signal = hot_sine(1_000.0, 1.0, 0.0, 48_000);
        let input = measured_peak(&signal, 8, 32);
        limiter.process(&mut signal, 48_000);
        let output = measured_peak(&signal, 8, 32);
        let applied = linear_to_db(input) - linear_to_db(output);
        assert!(
            (limiter.gain_reduction_db() - applied).abs() <= 0.2,
            "reported GR {} vs applied {applied}",
            limiter.gain_reduction_db()
        );
    }

    #[test]
    fn gain_reduction_metering_reports_current_max_margin_and_distribution() {
        let mut limiter = Limiter::new(48_000);
        limiter.set_param("ceiling", -6.0);
        limiter.set_param("true_peak", 1.0);
        limiter.set_param("release", 100.0);
        let mut signal = hot_sine(1_000.0, 1.0, 0.0, 24_000);
        limiter.process(&mut signal, 48_000);

        let summary = limiter.gain_reduction_summary();
        assert!(limiter.gain_reduction_db() > 0.0);
        assert!((limiter.max_gain_reduction_db() - summary.max_db).abs() < 1e-9);
        assert!(summary.max_db > limiter.gain_reduction_db() - 1e-9);
        assert!((summary.mean_db - summary.p50_db).abs() < 1.5);
        assert!(summary.limited_fraction > 0.9);
        // The 1 kHz tone sits at its steady reduction: margin is tiny but the
        // detector never reports more than the ceiling input.
        let margin = limiter.ceiling_margin_db();
        assert!((margin + 6.0).abs() < 0.05, "margin {margin}");
        let peak_db = limiter.max_input_peak_dbfs();
        assert!((peak_db - 0.0).abs() < 0.05, "input peak {peak_db}");
    }

    #[test]
    fn lookahead_opens_the_attack_before_the_peak_reaches_the_output() {
        let mut limiter = Limiter::new(48_000);
        limiter.set_param("ceiling", -6.0);
        limiter.set_param("lookahead", 5.0);
        limiter.set_param("release_mode", 1.0);
        let latency = limiter.latency_samples() as usize;
        assert_eq!(latency, 240);

        let frames = 2048;
        let impulse_frame = 1000;
        let mut first_gr = None;
        let mut first_output = None;
        let mut out = Vec::with_capacity(frames);
        for frame in 0..frames {
            let mut buffer = [0.0_f32; 2];
            if frame == impulse_frame {
                buffer[0] = 1.0;
                buffer[1] = -1.0;
            }
            limiter.process(&mut buffer, 48_000);
            if first_gr.is_none() && limiter.gain_reduction_db() > 0.0 {
                first_gr = Some(frame);
            }
            if first_output.is_none() && buffer[0].abs() > 1e-9 {
                first_output = Some(frame);
            }
            out.extend_from_slice(&buffer);
        }
        assert_eq!(first_gr, Some(impulse_frame));
        assert_eq!(first_output, Some(impulse_frame + latency));
        assert_eq!(
            first_output.unwrap() - first_gr.unwrap(),
            latency,
            "attack must open one lookahead ahead of the output peak"
        );
        assert!(measured_peak(&out, 4, 32) <= db_to_lin(-6.0) + 1e-3);
    }

    #[test]
    fn signal_below_the_ceiling_passes_unchanged() {
        let input = hot_sine(997.0, 0.4, 0.3, 8192);
        let mut zero_lookahead = input.clone();
        let mut limiter = Limiter::new(48_000);
        limiter.set_param("lookahead", 0.0);
        limiter.process(&mut zero_lookahead, 48_000);
        assert_eq!(zero_lookahead, input, "quiet signal must be bit-identical");

        let mut delayed = input.clone();
        let mut limiter = Limiter::new(48_000);
        limiter.set_param("lookahead", 5.0);
        limiter.process(&mut delayed, 48_000);
        let latency = limiter.latency_samples() as usize * 2;
        assert_eq!(&delayed[latency..], &input[..input.len() - latency]);
        assert!(delayed[..latency].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn release_is_monotonic_after_the_peak_ends() {
        for release_mode in [0.0, 1.0] {
            let mut limiter = Limiter::new(48_000);
            limiter.set_param("ceiling", -3.0);
            limiter.set_param("release", 60.0);
            limiter.set_param("release_mode", release_mode);
            let mut burst = hot_sine(1_000.0, 1.0, 0.0, 4800);
            limiter.process(&mut burst, 48_000);
            let mut silence = vec![0.0_f32; 48_000];
            let mut previous = limiter.gain_reduction_db();
            assert!(previous > 0.0);
            for frame in silence.chunks_exact_mut(2) {
                limiter.process(frame, 48_000);
                let gr = limiter.gain_reduction_db();
                assert!(
                    gr <= previous + 1e-9,
                    "release mode {release_mode} rose from {previous} to {gr}"
                );
                previous = gr;
            }
            assert!(previous < 0.01, "tail did not settle: {previous} dB");
        }
    }

    /// Frames for the envelope to fall from 6 dB to 1 dB after a burst.
    fn release_tail_frames(limiter: &mut Limiter, amplitude: f64) -> usize {
        let mut burst = hot_sine(1_000.0, amplitude, 0.0, 9600);
        limiter.process(&mut burst, 48_000);
        let mut frames = 0;
        let mut start = None;
        while frames < 96_000 {
            let mut buffer = [0.0_f32; 2];
            limiter.process(&mut buffer, 48_000);
            let gr = limiter.gain_reduction_db();
            if start.is_none() && gr <= 6.0 {
                start = Some(frames);
            }
            if let Some(start) = start
                && gr <= 1.0
            {
                return frames - start;
            }
            frames += 1;
        }
        frames
    }

    #[test]
    fn auto_release_tail_depends_on_the_program_but_fast_does_not() {
        let fast_shallow = {
            let mut limiter = Limiter::new(48_000);
            limiter.set_param("ceiling", -3.0);
            limiter.set_param("release", 60.0);
            limiter.set_param("release_mode", 0.0);
            release_tail_frames(&mut limiter, 1.78)
        };
        let fast_deep = {
            let mut limiter = Limiter::new(48_000);
            limiter.set_param("ceiling", -3.0);
            limiter.set_param("release", 60.0);
            limiter.set_param("release_mode", 0.0);
            release_tail_frames(&mut limiter, 7.08)
        };
        let auto_shallow = {
            let mut limiter = Limiter::new(48_000);
            limiter.set_param("ceiling", -3.0);
            limiter.set_param("release", 60.0);
            limiter.set_param("release_mode", 1.0);
            release_tail_frames(&mut limiter, 1.78)
        };
        let auto_deep = {
            let mut limiter = Limiter::new(48_000);
            limiter.set_param("ceiling", -3.0);
            limiter.set_param("release", 60.0);
            limiter.set_param("release_mode", 1.0);
            release_tail_frames(&mut limiter, 7.08)
        };
        assert!(
            fast_deep.abs_diff(fast_shallow) <= fast_shallow / 20,
            "fast release should ignore program depth: {fast_shallow} vs {fast_deep}"
        );
        assert!(
            auto_deep > auto_shallow * 12 / 10,
            "auto release tail must grow with depth: {auto_shallow} vs {auto_deep}"
        );
        println!(
            "release tail 6->1 dB in ms: fast {:.1} (shallow) / {:.1} (deep), auto {:.1} (shallow) / {:.1} (deep)",
            fast_shallow as f64 / 48.0,
            fast_deep as f64 / 48.0,
            auto_shallow as f64 / 48.0,
            auto_deep as f64 / 48.0
        );
    }

    #[test]
    fn steady_tone_limit_does_not_pump() {
        let mut limiter = Limiter::new(48_000);
        limiter.set_param("ceiling", -3.0);
        limiter.set_param("release", 50.0);
        limiter.set_param("release_mode", 1.0);
        let mut tone = hot_sine(1_000.0, 1.0, 0.0, 96_000);
        limiter.process(&mut tone, 48_000);
        // Skip the attack and the program-memory ramp-in, then measure ripple.
        let settled: Vec<f64> = tone
            .chunks_exact(2)
            .skip(12_000)
            .collect::<Vec<_>>()
            .chunks(480)
            .map(|block| {
                let power =
                    block.iter().map(|frame| frame[0] * frame[0]).sum::<f32>() / block.len() as f32;
                linear_to_db((power.sqrt() as f64).max(1e-12))
            })
            .collect();
        let first = settled[0];
        let ripple = settled
            .iter()
            .fold(0.0_f64, |ripple, level| ripple.max((level - first).abs()));
        assert!(ripple < 0.01, "steady-tone pumping ripple {ripple} dB");
    }

    #[test]
    fn partition_and_reset_are_exact() {
        let input: Vec<f32> = (0..4096).map(|i| (i as f32 * 0.17).sin() * 3.0).collect();
        let mut whole = input.clone();
        let mut limiter = Limiter::new(48_000);
        limiter.process(&mut whole, 48_000);
        limiter.reset();
        let mut partitioned = input;
        for block in partitioned.chunks_mut(34) {
            limiter.process(block, 48_000);
        }
        assert_eq!(whole, partitioned);
    }

    fn digest(buffer: &[f32]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for sample in buffer {
            hash ^= sample.to_bits() as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        hash
    }

    fn program(frames: usize, amplitude: f64) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f64 / 48_000.0;
                let left = (2.0 * std::f64::consts::PI * 997.0 * t).sin() * amplitude;
                let right = (2.0 * std::f64::consts::PI * 997.0 * t + 0.4).sin() * amplitude * 0.9;
                [left as f32, right as f32]
            })
            .collect()
    }

    fn faded_sine(frequency: f64, amplitude: f64, phase: f64, frames: usize) -> Vec<f32> {
        let fade = 4_800;
        (0..frames)
            .flat_map(|i| {
                let t = i as f64 / 48_000.0;
                let window = if i < fade {
                    0.5 - 0.5 * (std::f64::consts::PI * i as f64 / fade as f64).cos()
                } else if i + fade > frames {
                    0.5 - 0.5 * (std::f64::consts::PI * (frames - 1 - i) as f64 / fade as f64).cos()
                } else {
                    1.0
                };
                let sample =
                    (2.0 * std::f64::consts::PI * frequency * t + phase).sin() * amplitude * window;
                [sample as f32, sample as f32]
            })
            .collect()
    }

    fn bass_note(frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f64 / 48_000.0;
                let sample = 2.0 * (2.0 * std::f64::consts::PI * 55.0 * t).sin()
                    + 0.4 * (2.0 * std::f64::consts::PI * 110.0 * t).sin();
                [sample as f32, (sample * 0.95) as f32]
            })
            .collect()
    }

    fn dense_mix(frames: usize) -> Vec<f32> {
        let mut state = 0x51ed_2f13u32;
        (0..frames)
            .flat_map(|i| {
                let mut noise = || {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 8) as f64 / 8_388_608.0 - 1.0
                };
                let t = i as f64 / 48_000.0;
                let beat = t % 0.5;
                let kick = if beat < 0.12 {
                    let sweep = 55.0 + 90.0 * (-beat / 0.02).exp();
                    (2.0 * std::f64::consts::PI * sweep * t).sin() * (-beat / 0.05).exp() * 1.6
                } else {
                    0.0
                };
                let snare_phase = t % 1.0;
                let snare = if snare_phase < 0.15 {
                    noise() * (-snare_phase / 0.04).exp() * 0.9
                } else {
                    0.0
                };
                let bass = 0.7 * (2.0 * std::f64::consts::PI * 82.4 * t).sin()
                    + 0.3 * (2.0 * std::f64::consts::PI * 164.8 * t).sin();
                let chord = 0.3 * (2.0 * std::f64::consts::PI * 261.6 * t).sin()
                    + 0.25 * (2.0 * std::f64::consts::PI * 329.6 * t).sin()
                    + 0.2 * (2.0 * std::f64::consts::PI * 392.0 * t).sin();
                let tail = if t > 3.75 {
                    0.6 * (2.0 * std::f64::consts::PI * 196.0 * t).sin()
                } else {
                    0.0
                };
                let left = kick + snare + bass + chord + tail + 0.12 * noise();
                let right = kick * 0.9 + snare + bass * 0.95 + chord * 1.05 + tail + 0.12 * noise();
                [left as f32, right as f32]
            })
            .collect()
    }

    fn style_limiter(release_mode: f64) -> Limiter {
        let mut limiter = Limiter::new(48_000);
        limiter.set_param("release_mode", release_mode);
        limiter
    }

    fn release_time_to_63_ms(mut limiter: Limiter, programme: &[f32]) -> f64 {
        let mut buffer = programme.to_vec();
        limiter.process(&mut buffer, 48_000);
        let initial = limiter.gain_reduction_db();
        assert!(initial > 0.0, "programme must engage the limiter");
        let target = initial * 0.37;
        for frame in 0..48_000 * 15 {
            let mut block = [0.0_f32; 2];
            limiter.process(&mut block, 48_000);
            if limiter.gain_reduction_db() <= target {
                return frame as f64 / 48.0;
            }
        }
        f64::NAN
    }

    fn gain_modulation_db(mut limiter: Limiter, programme: &[f32], skip_frames: usize) -> f64 {
        let mut values = Vec::new();
        for (index, frame) in programme.chunks_exact(2).enumerate() {
            let mut block = [frame[0], frame[1]];
            limiter.process(&mut block, 48_000);
            if index >= skip_frames {
                values.push(limiter.gain_reduction_db());
            }
        }
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        (values
            .iter()
            .map(|value| (value - mean) * (value - mean))
            .sum::<f64>()
            / values.len() as f64)
            .sqrt()
    }

    #[test]
    fn defaults_keep_the_frozen_render() {
        let mut input = program(24_000, 1.9);
        input.extend(program(72_000, 0.05));
        let mut limiter = Limiter::new(48_000);
        assert_eq!(limiter.get_param("oversampling"), Some(0.0));
        limiter.process(&mut input, 48_000);
        assert_eq!(digest(&input), 0xee57_a306_6c2e_9db2);
    }

    #[test]
    fn oversampling_is_a_parameter() {
        let mut limiter = Limiter::new(48_000);
        assert_eq!(limiter.oversampler.phases(), 4);
        limiter.set_param("oversampling", 1.0);
        assert_eq!(limiter.get_param("oversampling"), Some(1.0));
        assert_eq!(limiter.oversampler.phases(), 8);
        limiter.set_param("oversampling", 2.0);
        assert_eq!(limiter.oversampler.phases(), 16);
        limiter.set_param("oversampling", 9.0);
        assert_eq!(limiter.get_param("oversampling"), Some(2.0));
        limiter.set_param("oversampling", 0.0);
        assert_eq!(limiter.oversampler.phases(), 4);
    }

    #[test]
    fn oversampling_improves_the_intersample_peak_estimate() {
        // 12 kHz at 45 degrees: sample peaks 0.707, true peak 1.0, and the
        // true peak falls between the 4x grid points.
        let input = faded_sine(12_000.0, 1.0, std::f64::consts::FRAC_PI_4, 96_000);
        let mut errors = Vec::new();
        for (label, phases) in [("4x", 4), ("8x", 8), ("16x", 16)] {
            let mut detector = TruePeakDetector::with_phases(phases);
            let mut peak = 0.0_f64;
            for frame in input.chunks_exact(2) {
                peak = peak.max(detector.peak(frame[0], frame[1]));
            }
            let error_db = linear_to_db(peak);
            println!("{label} true-peak error {error_db:+.4} dB");
            errors.push(error_db);
        }
        assert!(
            errors[0] < -0.1,
            "the 4x grid must miss this peak: {} dB",
            errors[0]
        );
        assert!(errors[1].abs() <= errors[0].abs());
        assert!(errors[2].abs() <= errors[1].abs());
    }

    #[test]
    fn release_styles_measure_distinctly() {
        let bass = bass_note(120_000);
        let mix = dense_mix(192_000);
        let mut rows = Vec::new();
        for (label, mode) in [
            ("fast", 0.0),
            ("auto", 1.0),
            ("punch", 2.0),
            ("sustain", 3.0),
        ] {
            let bass_release = release_time_to_63_ms(style_limiter(mode), &bass);
            let bass_modulation = gain_modulation_db(style_limiter(mode), &bass, 24_000);
            let mix_release = release_time_to_63_ms(style_limiter(mode), &mix);
            let mix_modulation = gain_modulation_db(style_limiter(mode), &mix, 24_000);
            println!(
                "{label:8} bass t63 {bass_release:7.1} ms mod {bass_modulation:.3} dB | \
                 mix t63 {mix_release:7.1} ms mod {mix_modulation:.3} dB"
            );
            rows.push((
                label,
                bass_release,
                bass_modulation,
                mix_release,
                mix_modulation,
            ));
        }
        let fast = rows[0];
        let auto = rows[1];
        let punch = rows[2];
        let sustain = rows[3];
        assert!(fast.3 < punch.3, "fast must release faster than punch");
        assert!(
            punch.3 < auto.3 * 0.95,
            "punch must release faster than auto"
        );
        assert!(
            sustain.3 > auto.3 * 1.05,
            "sustain must release slower than auto"
        );
        for row in &rows {
            assert!(row.1.is_finite() && row.1 > 0.0);
            assert!(row.3.is_finite() && row.3 > 0.0);
            assert!(row.2 < 0.05, "steady bass note must not pump: {row:?}");
            assert!(row.4 > 0.01, "dense mix must modulate the gain: {row:?}");
        }
    }

    #[test]
    #[ignore]
    fn oversampling_cost_report() {
        let frames = 48_000 * 5;
        let input = program(frames, 1.9);
        for (label, index) in [("4x", 0.0), ("8x", 1.0), ("16x", 2.0)] {
            let mut best = f64::INFINITY;
            for _ in 0..3 {
                let mut buffer = input.clone();
                let mut limiter = Limiter::new(48_000);
                limiter.set_param("oversampling", index);
                let start = std::time::Instant::now();
                limiter.process(&mut buffer, 48_000);
                best = best.min(start.elapsed().as_secs_f64());
            }
            let ns_per_frame = best * 1.0e9 / frames as f64;
            println!(
                "{label:4} {ns_per_frame:8.1} ns/frame  {:.1}x realtime",
                (frames as f64 / 48_000.0) / best
            );
        }
    }

    #[test]
    fn two_instances_are_bit_identical() {
        let input: Vec<f32> = (0..48_000)
            .flat_map(|i| {
                let t = i as f64 / 48_000.0;
                let sample = (2.0 * std::f64::consts::PI * 220.0 * t).sin()
                    + 0.4 * (2.0 * std::f64::consts::PI * 3_100.0 * t).sin();
                [sample as f32, (sample * 0.9) as f32]
            })
            .collect();
        let mut first = input.clone();
        let mut second = input;
        let mut a = Limiter::new(48_000);
        let mut b = Limiter::new(48_000);
        a.process(&mut first, 48_000);
        b.process(&mut second, 48_000);
        assert_eq!(first, second);
        assert_eq!(a.gain_reduction_summary(), b.gain_reduction_summary());
    }

    #[test]
    fn link_is_a_parameter_clamped_to_its_range() {
        let mut limiter = Limiter::new(48_000);
        assert_eq!(limiter.get_param("link"), Some(100.0));
        limiter.set_param("link", 40.0);
        assert_eq!(limiter.get_param("link"), Some(40.0));
        limiter.set_param("link", -5.0);
        assert_eq!(limiter.get_param("link"), Some(0.0));
        limiter.set_param("link", 250.0);
        assert_eq!(limiter.get_param("link"), Some(100.0));
    }

    fn burst_limiter(link: f64) -> Limiter {
        let mut limiter = Limiter::new(48_000);
        limiter.set_param("ceiling", -1.0);
        limiter.set_param("release", 200.0);
        limiter.set_param("link", link);
        limiter
    }

    fn assert_ceiling_holds(signal: &[f32]) {
        for channel in 0..2 {
            let peak = measured_channel_peak(signal, channel, 4, 32);
            assert!(
                peak <= db_to_lin(-1.0) + 1e-3,
                "channel {channel} true peak {peak} exceeds the ceiling"
            );
        }
    }

    #[test]
    fn full_link_ducks_both_channels_equally() {
        let input = panned_burst(48_000);
        let mut signal = input.clone();
        burst_limiter(100.0).process(&mut signal, 48_000);
        let left = channel_gain_reduction_db(&input, &signal, 0);
        let right = channel_gain_reduction_db(&input, &signal, 1);
        println!("link 100 %: left {left:.3} dB, right {right:.3} dB");
        assert!(left > 6.0, "left channel must be limited: {left} dB");
        assert!(
            (left - right).abs() <= 0.1,
            "link 100 %: left {left} dB, right {right} dB"
        );
        assert_ceiling_holds(&signal);
    }

    #[test]
    fn zero_link_limits_each_channel_on_its_own_level() {
        let input = panned_burst(48_000);
        let mut signal = input.clone();
        burst_limiter(0.0).process(&mut signal, 48_000);
        let left = channel_gain_reduction_db(&input, &signal, 0);
        let right = channel_gain_reduction_db(&input, &signal, 1);
        println!("link 0 %: left {left:.3} dB, right {right:.3} dB");
        assert!(left > 6.0, "left channel must still be limited: {left} dB");
        assert!(
            right.abs() <= 0.01,
            "link 0 %: right channel moved by {right} dB"
        );
        assert_ceiling_holds(&signal);
    }

    #[test]
    fn partial_link_blends_the_two_detectors() {
        let input = panned_burst(48_000);
        let mut linked = input.clone();
        burst_limiter(100.0).process(&mut linked, 48_000);
        let linked_left = channel_gain_reduction_db(&input, &linked, 0);
        let linked_right = channel_gain_reduction_db(&input, &linked, 1);

        let mut half = input.clone();
        burst_limiter(50.0).process(&mut half, 48_000);
        let left = channel_gain_reduction_db(&input, &half, 0);
        let right = channel_gain_reduction_db(&input, &half, 1);

        // The left channel's own detector equals the linked one.
        assert!(
            (left - linked_left).abs() <= 0.1,
            "left {left} vs {linked_left}"
        );
        // The right channel blends the linked gain with unity in amplitude.
        let blend = 0.5 * 10.0_f64.powf(-linked_right / 20.0) + 0.5;
        let predicted = -20.0 * blend.log10();
        println!("link 50 %: left {left:.3} dB, right {right:.3} dB (predicted {predicted:.3})");
        assert!(
            (right - predicted).abs() <= 0.1,
            "link 50 %: right {right} dB, predicted {predicted} dB"
        );
        assert!(right > 0.01 && right < linked_right - 0.5);
        assert_ceiling_holds(&half);
    }
}
