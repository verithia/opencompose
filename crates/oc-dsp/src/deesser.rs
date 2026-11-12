//! De-esser: 4th-order Butterworth sidechain, hard-threshold `Range` gain law,
//! zero-phase split band and a mid-linked peak detector.
//!
//! Wide band scales the signal; split band adds a zero-phase power-complementary
//! high band, so neutral gain is an exact 512-sample delay. The shipped defaults
//! are absolute threshold, sidechain detection, wide band and split at `HP/4`.

use std::sync::Arc;

use rustfft::Fft;
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

use super::AudioEffect;
use super::biquad::{BiquadFilter, FilterType};
use super::detector::{db_to_linear, linear_to_db};

const MAX_LOOKAHEAD_MS: f64 = 15.0;
const LOOKAHEAD_MS: f64 = 15.0;
const SIDECHAIN_OVERSAMPLE: usize = 4;
const SPLIT_DELAY: usize = 512;
/// Lowest split corner the split filter is sized for (`high_frequency` 2 kHz / 4).
const MIN_SPLIT_HZ: f64 = 500.0;
const BALLISTIC_POLES: usize = 4;
const PEAK_HOLD_MS: f64 = 1.0;
const BUTTERWORTH_Q_LOW: f64 = 0.541_196_1;
const BUTTERWORTH_Q_HIGH: f64 = 1.306_563;
const RESONANCE_Q: f64 = 1.0;
const BALLISTIC_T63_PER_TAU: f64 = 4.34;
const DEFAULT_HIGH_HZ: f64 = 7_500.0;
const DEFAULT_LOW_HZ: f64 = 14_000.0;
const RELATIVE_ATTACK_MS: f64 = 200.0;
const RELATIVE_RELEASE_MS: f64 = 1_000.0;
const SILENCE_POWER: f64 = 1e-30;

/// Short-time analysis frame and hop for the band estimator and the segment
/// detector. The hop is the control rate, the frame is the frequency window.
const ANALYSIS_FRAME: usize = 1_024;
const ANALYSIS_HOP: usize = 256;
const SIBILANCE_MIN_HZ: f64 = 4_000.0;
const SIBILANCE_MAX_HZ: f64 = 16_000.0;
const BODY_MIN_HZ: f64 = 200.0;
const BODY_MAX_HZ: f64 = 2_000.0;
/// The step law puts the band at `peak / 1.4`; the estimator follows it.
const BAND_TO_PEAK: f64 = 1.4;
/// Sidechain low-pass / high-pass ratio shared with the fixed band law.
const BAND_TOP_RATIO: f64 = DEFAULT_LOW_HZ / DEFAULT_HIGH_HZ;
const MIN_BAND_HZ: f64 = 3_000.0;
const MAX_BAND_HZ: f64 = 12_000.0;
/// A frame is sibilant when the high band is this share of high plus body.
const SIBILANT_SHARE_DB: f64 = -12.0;
/// Hysteresis: leave a segment only below the exit share, after confirm frames.
const SIBILANT_EXIT_DB: f64 = -18.0;
const SEGMENT_ENTER_FRAMES: usize = 2;
const SEGMENT_EXIT_FRAMES: usize = 2;
const SEGMENT_MIN_DWELL_FRAMES: usize = 4;
const SEGMENT_ATTACK_MS: f64 = 3.0;
const SEGMENT_RELEASE_MS: f64 = 25.0;
const BAND_FLOOR_DBFS: f64 = -60.0;
const FRAME_SILENCE_RMS: f64 = 1e-4;
/// Leaky accumulator: recent sibilant frames decide the band, slow to forget.
const ESTIMATE_LEAK: f64 = 0.999;
const ESTIMATE_MIN_FRAMES: usize = 8;
const ESTIMATE_DECISION_FRAMES: usize = 8;
/// A new decision replaces the held band only when it moves by this much.
const BAND_CHANGE_RATIO: f64 = 0.08;

/// Measured peak boost of the narrow sidechain band vs `LP/HP` corner ratio.
const BAND_RATIO: [f64; 6] = [6.67, 2.80, 2.00, 1.87, 1.20, 1.10];
const BAND_BOOST_DB: [f64; 6] = [0.0, 1.7, 6.8, 7.7, 18.9, 21.7];

/// Whether a parameter id names a frequency.
pub fn frequency_param(name: &str) -> bool {
    matches!(name, "high_frequency" | "low_frequency" | "band_override")
}

/// Whether a parameter id names a dB value.
pub fn db_param(name: &str) -> bool {
    matches!(name, "threshold" | "range" | "relative_threshold")
}

/// Whether a parameter id names a time in milliseconds.
pub fn time_ms_param(name: &str) -> bool {
    matches!(name, "attack" | "release" | "lookahead")
}

/// Whether a parameter id names a percentage.
pub fn percent_param(name: &str) -> bool {
    name == "stereo_link"
}

/// Map a band-processing label to its selector code.
pub fn band_processing_code(name: &str) -> Option<f64> {
    match normalize_choice(name).as_str() {
        "wide_band" | "wide" | "full_band" | "full" => Some(0.0),
        "split_band" | "split" | "high_frequency_only" => Some(1.0),
        _ => None,
    }
}

/// Map a threshold-mode label to its selector code.
pub fn threshold_mode_code(name: &str) -> Option<f64> {
    match normalize_choice(name).as_str() {
        "absolute" | "fixed" => Some(0.0),
        "relative" | "program" => Some(1.0),
        _ => None,
    }
}

/// Map a detector label to its selector code.
pub fn detector_code(name: &str) -> Option<f64> {
    match normalize_choice(name).as_str() {
        "sidechain" | "before_split" | "pre_split" => Some(0.0),
        "in_band" | "inband" => Some(1.0),
        _ => None,
    }
}

/// Map a band-estimation label to its selector code.
pub fn band_estimation_code(name: &str) -> Option<f64> {
    match normalize_choice(name).as_str() {
        "off" | "none" | "manual" => Some(0.0),
        "learn" | "running" | "on" => Some(1.0),
        _ => None,
    }
}

/// Map a segment-detection label to its selector code.
pub fn segment_detection_code(name: &str) -> Option<f64> {
    match normalize_choice(name).as_str() {
        "off" | "none" => Some(0.0),
        "on" | "segments" | "detect" => Some(1.0),
        _ => None,
    }
}

/// Map a sibilance-band label to its selector code.
pub fn sibilance_band_code(name: &str) -> Option<f64> {
    match normalize_choice(name).as_str() {
        "auto" | "hp_over_4" => Some(0.0),
        "5_khz" | "5_k" | "5000" => Some(1.0),
        "6_khz" | "6_k" | "6000" => Some(2.0),
        "7_khz" | "7_k" | "7000" => Some(3.0),
        "8_khz" | "8_k" | "8000" => Some(4.0),
        "9_khz" | "9_k" | "9000" => Some(5.0),
        _ => None,
    }
}

fn sibilance_band_hz(index: usize) -> f64 {
    4_000.0 + 1_000.0 * index as f64
}

fn normalize_choice(text: &str) -> String {
    text.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c == ' ' || c == '-' { '_' } else { c })
        .collect()
}

fn band_boost_db(ratio: f64) -> f64 {
    if ratio >= BAND_RATIO[0] {
        return 0.0;
    }
    if ratio <= BAND_RATIO[5] {
        return BAND_BOOST_DB[5];
    }
    for index in 0..BAND_RATIO.len() - 1 {
        if (BAND_RATIO[index + 1]..=BAND_RATIO[index]).contains(&ratio) {
            let span = BAND_RATIO[index].ln() - BAND_RATIO[index + 1].ln();
            let t = (BAND_RATIO[index].ln() - ratio.ln()) / span;
            return BAND_BOOST_DB[index] + t * (BAND_BOOST_DB[index + 1] - BAND_BOOST_DB[index]);
        }
    }
    0.0
}

/// Linear-phase `H_split = x^2/(1+x^2)` at `fc`, group delay 512.
#[cfg(test)]
fn design_split_fir(fc: f64, sample_rate: u32) -> (Vec<f64>, usize) {
    let half = split_half_for(fc, sample_rate);
    let mut fir = vec![0.0; 2 * half + 1];
    let mut kernel = vec![0.0; half + 1];
    design_split_fir_into(fc, sample_rate, &mut fir, &mut kernel);
    (fir, half)
}

fn split_half_for(fc: f64, sample_rate: u32) -> usize {
    let fc = fc.max(20.0);
    let step = std::f64::consts::TAU * fc / sample_rate as f64;
    ((14.0 / step).ceil() as usize).clamp(8, SPLIT_DELAY)
}

/// Same coefficients as `design_split_fir`, written into preallocated storage.
fn design_split_fir_into(fc: f64, sample_rate: u32, fir: &mut [f64], kernel: &mut [f64]) -> usize {
    let sample_rate = sample_rate as f64;
    let fc = fc.max(20.0);
    let bins = 2 * SPLIT_DELAY + 1;
    let step = std::f64::consts::TAU * fc / sample_rate;
    let half = ((14.0 / step).ceil() as usize).clamp(8, SPLIT_DELAY);
    for (offset, slot) in kernel[..=half].iter_mut().enumerate() {
        let mut sum = 0.0;
        for bin in 0..=SPLIT_DELAY {
            let freq = bin as f64 * sample_rate / bins as f64;
            let x = freq / fc;
            let weight = if bin == 0 { 1.0 } else { 2.0 };
            let angle = std::f64::consts::TAU * bin as f64 * offset as f64 / bins as f64;
            sum += weight * (x * x / (1.0 + x * x)) * angle.cos();
        }
        *slot = sum / bins as f64;
    }
    for (index, value) in fir[..=2 * half].iter_mut().enumerate() {
        let offset = (index as isize - half as isize).unsigned_abs();
        *value = kernel[offset];
    }
    half
}

/// Sibilant band chosen for a take or held by the running estimator.
///
/// `band_hz` is the sidechain high-pass and split corner (the unit the fixed
/// `sibilance_band` steps use), `top_hz` is the sidechain low-pass corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SibilanceBand {
    /// Energy centroid of the sibilant frames, Hz.
    pub peak_hz: f64,
    /// Sidechain high-pass and split corner, Hz.
    pub band_hz: f64,
    /// Sidechain low-pass corner, Hz.
    pub top_hz: f64,
    /// Frames that looked sibilant.
    pub frames: usize,
}

fn hann_window(len: usize) -> Vec<f64> {
    (0..len)
        .map(|index| 0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / (len - 1) as f64).cos())
        .collect()
}

fn bins_for(sample_rate: u32, fft_len: usize, low_hz: f64, high_hz: f64) -> (usize, usize) {
    let scale = fft_len as f64 / sample_rate as f64;
    let low = (low_hz * scale).round() as usize;
    let high = ((high_hz * scale).round() as usize).min(fft_len / 2);
    (low.min(high), high)
}

fn bin_power(spectrum: &[Complex<f64>], low: usize, high: usize) -> f64 {
    spectrum[low..=high].iter().map(|bin| bin.norm_sqr()).sum()
}

fn share_db(high: f64, low: f64) -> f64 {
    let total = high + low;
    if total <= SILENCE_POWER {
        -200.0
    } else {
        10.0 * (high / total).max(1e-20).log10()
    }
}

/// Band RMS in dBFS from the positive-frequency bin sum of a Hann frame.
fn band_rms_db(power: f64, fft_len: usize) -> f64 {
    let mean_square = 2.0 * power / (fft_len * fft_len) as f64;
    10.0 * mean_square.max(SILENCE_POWER).log10()
}

fn band_from_peak(peak_hz: f64, sample_rate: u32) -> Option<SibilanceBand> {
    let nyquist = sample_rate as f64 * 0.5;
    let top_limit = SIBILANCE_MAX_HZ.min(nyquist * 0.9);
    if !(SIBILANCE_MIN_HZ..=top_limit).contains(&peak_hz) {
        return None;
    }
    let band = (peak_hz / BAND_TO_PEAK).clamp(MIN_BAND_HZ, MAX_BAND_HZ.min(nyquist * 0.8));
    let top = (band * BAND_TOP_RATIO).min(top_limit).max(band * 1.2);
    Some(SibilanceBand {
        peak_hz,
        band_hz: band,
        top_hz: top,
        frames: 0,
    })
}

fn finalize_band(
    bins: &[f64],
    fft_len: usize,
    frames: usize,
    sample_rate: u32,
) -> Option<SibilanceBand> {
    if frames < ESTIMATE_MIN_FRAMES {
        return None;
    }
    let total: f64 = bins.iter().sum();
    if total <= 0.0 || !total.is_finite() {
        return None;
    }
    let bin_hz = sample_rate as f64 / fft_len as f64;
    let peak = bins
        .iter()
        .enumerate()
        .map(|(index, power)| index as f64 * bin_hz * power)
        .sum::<f64>()
        / total;
    let mut band = band_from_peak(peak, sample_rate)?;
    band.frames = frames;
    Some(band)
}

/// Estimate the sibilant band of a whole take from mono samples.
///
/// Returns `None` when no frame looks sibilant: the caller keeps the shipped
/// default band. Deterministic and allocation is allowed (offline path).
pub fn estimate_sibilance_band(signal: &[f32], sample_rate: u32) -> Option<SibilanceBand> {
    if sample_rate < 8_000 || signal.len() < ANALYSIS_FRAME {
        return None;
    }
    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(ANALYSIS_FRAME);
    let mut scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
    let mut frame = vec![Complex::new(0.0, 0.0); ANALYSIS_FRAME];
    let window = hann_window(ANALYSIS_FRAME);
    let (estimate_low, estimate_high) = bins_for(
        sample_rate,
        ANALYSIS_FRAME,
        SIBILANCE_MIN_HZ,
        SIBILANCE_MAX_HZ,
    );
    let (body_low, body_high) = bins_for(sample_rate, ANALYSIS_FRAME, BODY_MIN_HZ, BODY_MAX_HZ);
    let mut bins = vec![0.0; ANALYSIS_FRAME / 2 + 1];
    let mut frames = 0;
    let mut start = 0;
    while start + ANALYSIS_FRAME <= signal.len() {
        let mut sum_sq = 0.0;
        for index in 0..ANALYSIS_FRAME {
            let sample = signal[start + index] as f64;
            sum_sq += sample * sample;
            frame[index] = Complex::new(sample * window[index], 0.0);
        }
        let rms = (sum_sq / ANALYSIS_FRAME as f64).sqrt();
        fft.process_with_scratch(&mut frame, &mut scratch);
        let high = bin_power(&frame, estimate_low, estimate_high);
        let body = bin_power(&frame, body_low, body_high);
        if rms >= FRAME_SILENCE_RMS && share_db(high, body) >= SIBILANT_SHARE_DB {
            for (slot, bin) in bins[estimate_low..=estimate_high]
                .iter_mut()
                .zip(&frame[estimate_low..=estimate_high])
            {
                *slot += bin.norm_sqr();
            }
            frames += 1;
        }
        start += ANALYSIS_HOP;
    }
    finalize_band(&bins, ANALYSIS_FRAME, frames, sample_rate)
}

/// Running band estimator plus sibilant-segment detector.
///
/// Everything is allocated here; `push` only fills preallocated buffers.
struct SibilanceAnalyzer {
    fft: Arc<dyn Fft<f64>>,
    window: Vec<f64>,
    ring: Vec<f64>,
    pos: usize,
    since_hop: usize,
    spectrum: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    bins: Vec<f64>,
    estimate_low: usize,
    estimate_high: usize,
    body_low: usize,
    body_high: usize,
    candidate_frames: usize,
    since_decision: usize,
    held: Option<SibilanceBand>,
    active: bool,
    enter_count: usize,
    exit_count: usize,
    dwell: usize,
    share_db: f64,
    high_db: f64,
    low_db: f64,
}

impl SibilanceAnalyzer {
    fn new(sample_rate: u32) -> Self {
        let mut planner = FftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(ANALYSIS_FRAME);
        let scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
        let (estimate_low, estimate_high) = bins_for(
            sample_rate,
            ANALYSIS_FRAME,
            SIBILANCE_MIN_HZ,
            SIBILANCE_MAX_HZ,
        );
        let (body_low, body_high) = bins_for(sample_rate, ANALYSIS_FRAME, BODY_MIN_HZ, BODY_MAX_HZ);
        Self {
            fft,
            window: hann_window(ANALYSIS_FRAME),
            ring: vec![0.0; ANALYSIS_FRAME],
            pos: 0,
            since_hop: 0,
            spectrum: vec![Complex::new(0.0, 0.0); ANALYSIS_FRAME],
            scratch,
            bins: vec![0.0; ANALYSIS_FRAME / 2 + 1],
            estimate_low,
            estimate_high,
            body_low,
            body_high,
            candidate_frames: 0,
            since_decision: 0,
            held: None,
            active: false,
            enter_count: 0,
            exit_count: 0,
            dwell: 0,
            share_db: -200.0,
            high_db: -200.0,
            low_db: -200.0,
        }
    }

    fn active(&self) -> bool {
        self.active
    }

    fn held(&self) -> Option<SibilanceBand> {
        self.held
    }

    fn reset(&mut self) {
        self.ring.fill(0.0);
        self.pos = 0;
        self.since_hop = 0;
        self.bins.fill(0.0);
        self.candidate_frames = 0;
        self.since_decision = 0;
        self.held = None;
        self.active = false;
        self.enter_count = 0;
        self.exit_count = 0;
        self.dwell = 0;
        self.share_db = -200.0;
        self.high_db = -200.0;
        self.low_db = -200.0;
    }

    fn reset_segments(&mut self) {
        self.active = false;
        self.enter_count = 0;
        self.exit_count = 0;
        self.dwell = 0;
        self.share_db = -200.0;
        self.high_db = -200.0;
        self.low_db = -200.0;
    }

    fn push(
        &mut self,
        sample: f64,
        sample_rate: u32,
        band_hz: f64,
        top_hz: f64,
        estimate: bool,
        detect: bool,
    ) -> Option<SibilanceBand> {
        self.ring[self.pos] = sample;
        self.pos = (self.pos + 1) % ANALYSIS_FRAME;
        self.since_hop += 1;
        if self.since_hop < ANALYSIS_HOP {
            return None;
        }
        self.since_hop = 0;

        let mut sum_sq = 0.0;
        for index in 0..ANALYSIS_FRAME {
            let value = self.ring[(self.pos + index) % ANALYSIS_FRAME];
            sum_sq += value * value;
            self.spectrum[index] = Complex::new(value * self.window[index], 0.0);
        }
        let rms = (sum_sq / ANALYSIS_FRAME as f64).sqrt();
        {
            let (spectrum, scratch) = (&mut self.spectrum, &mut self.scratch);
            self.fft.process_with_scratch(spectrum, scratch);
        }
        let (detect_low, detect_high) = bins_for(
            sample_rate,
            ANALYSIS_FRAME,
            band_hz,
            top_hz.min(sample_rate as f64 * 0.45),
        );
        let high = bin_power(&self.spectrum, detect_low, detect_high);
        let body = bin_power(&self.spectrum, self.body_low, self.body_high);
        self.share_db = share_db(high, body);
        self.high_db = band_rms_db(high, ANALYSIS_FRAME);
        self.low_db = band_rms_db(body, ANALYSIS_FRAME);

        let mut decided = None;
        if estimate {
            for slot in &mut self.bins {
                *slot *= ESTIMATE_LEAK;
            }
            if rms >= FRAME_SILENCE_RMS && self.share_db >= SIBILANT_SHARE_DB {
                for (slot, bin) in self.bins[self.estimate_low..=self.estimate_high]
                    .iter_mut()
                    .zip(&self.spectrum[self.estimate_low..=self.estimate_high])
                {
                    *slot += bin.norm_sqr();
                }
                self.candidate_frames += 1;
            }
            self.since_decision += 1;
            if self.since_decision >= ESTIMATE_DECISION_FRAMES
                && self.candidate_frames >= ESTIMATE_MIN_FRAMES
            {
                self.since_decision = 0;
                if let Some(band) = finalize_band(
                    &self.bins,
                    ANALYSIS_FRAME,
                    self.candidate_frames,
                    sample_rate,
                ) {
                    let moved = match self.held {
                        Some(old) => (band.band_hz / old.band_hz - 1.0).abs() > BAND_CHANGE_RATIO,
                        None => true,
                    };
                    if moved {
                        self.held = Some(band);
                        decided = Some(band);
                    }
                }
                self.candidate_frames = 0;
            }
        }

        if detect {
            let present = rms >= FRAME_SILENCE_RMS
                && self.high_db > BAND_FLOOR_DBFS
                && self.low_db > BAND_FLOOR_DBFS;
            if !self.active {
                if present && self.share_db >= SIBILANT_SHARE_DB {
                    self.enter_count += 1;
                } else {
                    self.enter_count = 0;
                }
                if self.enter_count >= SEGMENT_ENTER_FRAMES {
                    self.active = true;
                    self.enter_count = 0;
                    self.exit_count = 0;
                    self.dwell = 0;
                }
            } else {
                self.dwell += 1;
                if !present || self.share_db < SIBILANT_EXIT_DB {
                    self.exit_count += 1;
                } else {
                    self.exit_count = 0;
                }
                if self.exit_count >= SEGMENT_EXIT_FRAMES && self.dwell >= SEGMENT_MIN_DWELL_FRAMES
                {
                    self.active = false;
                    self.exit_count = 0;
                }
            }
        }
        decided
    }
}

pub struct DeEsser {
    threshold_db: f64,
    relative_threshold_db: f64,
    relative: bool,
    in_band: bool,
    sibilance_band: usize,
    range_db: f64,
    attack_ms: f64,
    release_ms: f64,
    high_frequency_hz: f64,
    low_frequency_hz: f64,
    split_band: bool,
    stereo_link: f64,
    lookahead_ms: f64,
    sample_rate: u32,
    lookahead_len: usize,
    lookahead_knob: usize,
    sidechain: [BiquadFilter; 5],
    peak_hold_len: usize,
    peak_hold: [Vec<f64>; 2],
    peak_hold_pos: usize,
    ballistic: [[f64; BALLISTIC_POLES]; 2],
    attack_coeff: f64,
    release_coeff: f64,
    program: f64,
    program_attack_coeff: f64,
    program_release_coeff: f64,
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    delay_pos: usize,
    split_fir: Vec<f64>,
    split_kernel: Vec<f64>,
    split_half: usize,
    split_len: usize,
    split_l: Vec<f64>,
    split_r: Vec<f64>,
    split_pos: usize,
    split_dry: [[f64; SPLIT_DELAY]; 2],
    split_dry_pos: usize,
    detect_split_l: Vec<f64>,
    detect_split_r: Vec<f64>,
    detect_split_pos: usize,
    band_estimation: bool,
    band_override_hz: f64,
    segment_detection: bool,
    analyzer: SibilanceAnalyzer,
    segment_attack_coeff: f64,
    segment_release_coeff: f64,
    segment_gate: f64,
    last_gr_db: f64,
}

impl DeEsser {
    pub fn new(sample_rate: u32) -> Self {
        let sidechain_rate = sample_rate * SIDECHAIN_OVERSAMPLE as u32;
        let make =
            |filter_type, freq, q| BiquadFilter::new(filter_type, freq, q, 0.0, sidechain_rate);
        let peak_hold_len = ((sample_rate as f64 * PEAK_HOLD_MS * 0.001).round() as usize).max(1);
        let mut de_esser = Self {
            threshold_db: -36.0,
            relative_threshold_db: -12.0,
            relative: false,
            in_band: false,
            sibilance_band: 0,
            range_db: 6.0,
            attack_ms: 11.0,
            release_ms: 15.5,
            high_frequency_hz: DEFAULT_HIGH_HZ,
            low_frequency_hz: DEFAULT_LOW_HZ,
            split_band: false,
            stereo_link: 1.0,
            lookahead_ms: 0.0,
            sample_rate,
            lookahead_len: 0,
            lookahead_knob: 0,
            sidechain: [
                make(FilterType::HighPass, DEFAULT_HIGH_HZ, BUTTERWORTH_Q_LOW),
                make(FilterType::HighPass, DEFAULT_HIGH_HZ, BUTTERWORTH_Q_HIGH),
                make(FilterType::LowPass, DEFAULT_LOW_HZ, BUTTERWORTH_Q_LOW),
                make(FilterType::LowPass, DEFAULT_LOW_HZ, BUTTERWORTH_Q_HIGH),
                make(
                    FilterType::Peak,
                    (DEFAULT_HIGH_HZ * DEFAULT_LOW_HZ).sqrt(),
                    RESONANCE_Q,
                ),
            ],
            peak_hold_len,
            peak_hold: [vec![0.0; peak_hold_len], vec![0.0; peak_hold_len]],
            peak_hold_pos: 0,
            ballistic: [[0.0; BALLISTIC_POLES]; 2],
            attack_coeff: 0.0,
            release_coeff: 0.0,
            program: 0.0,
            program_attack_coeff: 0.0,
            program_release_coeff: 0.0,
            delay_l: vec![0.0; 1],
            delay_r: vec![0.0; 1],
            delay_pos: 0,
            split_fir: Vec::new(),
            split_kernel: Vec::new(),
            split_half: 0,
            split_len: 0,
            split_l: Vec::new(),
            split_r: Vec::new(),
            split_pos: 0,
            split_dry: [[0.0; SPLIT_DELAY]; 2],
            split_dry_pos: 0,
            detect_split_l: Vec::new(),
            detect_split_r: Vec::new(),
            detect_split_pos: 0,
            band_estimation: false,
            band_override_hz: 0.0,
            segment_detection: false,
            analyzer: SibilanceAnalyzer::new(sample_rate),
            segment_attack_coeff: 0.0,
            segment_release_coeff: 0.0,
            segment_gate: 0.0,
            last_gr_db: 0.0,
        };
        de_esser.allocate_split();
        de_esser.update_segment_ballistics();
        de_esser.update_sidechain();
        de_esser.update_ballistics();
        de_esser.update_program();
        de_esser.update_split(false);
        de_esser
    }

    fn update_segment_ballistics(&mut self) {
        let rate = self.sample_rate as f64 * 0.001;
        let attack = (SEGMENT_ATTACK_MS / BALLISTIC_T63_PER_TAU).max(0.01);
        let release = (SEGMENT_RELEASE_MS / BALLISTIC_T63_PER_TAU).max(0.01);
        self.segment_attack_coeff = (-1.0 / (attack * rate)).exp();
        self.segment_release_coeff = (-1.0 / (release * rate)).exp();
    }

    fn allocate_split(&mut self) {
        let max_half = split_half_for(MIN_SPLIT_HZ, self.sample_rate);
        self.split_fir = vec![0.0; 2 * max_half + 1];
        self.split_kernel = vec![0.0; max_half + 1];
        let len = SPLIT_DELAY + max_half + 1;
        self.split_len = len;
        self.split_l = vec![0.0; 2 * len];
        self.split_r = vec![0.0; 2 * len];
        self.detect_split_l = vec![0.0; 2 * len];
        self.detect_split_r = vec![0.0; 2 * len];
        self.split_pos = 0;
        self.detect_split_pos = 0;
    }

    fn band_selection(&self) -> Option<(f64, f64)> {
        if self.band_override_hz > 0.0 {
            let high = self.band_override_hz;
            let low = (high * BAND_TOP_RATIO).min(20_000.0).max(high * 1.01);
            return Some((high, low));
        }
        if self.band_estimation {
            if let Some(band) = self.analyzer.held() {
                return Some((band.band_hz, band.top_hz));
            }
            return None;
        }
        if self.sibilance_band > 0 {
            let high = sibilance_band_hz(self.sibilance_band);
            let low = (high * DEFAULT_LOW_HZ / DEFAULT_HIGH_HZ)
                .min(20_000.0)
                .max(high * 1.01);
            return Some((high, low));
        }
        None
    }

    fn effective_corners(&self) -> (f64, f64) {
        if let Some((high, low)) = self.band_selection() {
            return (high, low);
        }
        let high = self
            .high_frequency_hz
            .min(self.low_frequency_hz * 0.99)
            .max(20.0);
        let low = self.low_frequency_hz.max(high * 1.01);
        (high, low)
    }

    fn split_corner_hz(&self) -> f64 {
        if let Some((high, _)) = self.band_selection() {
            high
        } else {
            self.effective_corners().0 / 4.0
        }
    }

    /// Effective sidechain corners: high pass first, low pass second.
    pub fn band_edges_hz(&self) -> (f64, f64) {
        self.effective_corners()
    }

    /// The band the running estimator has locked, or the manual override.
    pub fn detected_band_hz(&self) -> Option<f64> {
        if self.band_override_hz > 0.0 {
            return Some(self.band_override_hz);
        }
        self.analyzer.held().map(|band| band.band_hz)
    }

    /// Energy centroid of the last accepted estimate, Hz.
    pub fn estimated_peak_hz(&self) -> Option<f64> {
        self.analyzer.held().map(|band| band.peak_hz)
    }

    /// True while the segment detector holds a sibilant segment.
    pub fn sibilant_active(&self) -> bool {
        self.analyzer.active()
    }

    fn update_sidechain(&mut self) {
        let (high, low) = self.effective_corners();
        let boost = band_boost_db(low / high);
        let center = (high * low).sqrt();
        self.sidechain[0].set_params(high, BUTTERWORTH_Q_LOW, 0.0);
        self.sidechain[1].set_params(high, BUTTERWORTH_Q_HIGH, 0.0);
        self.sidechain[2].set_params(low, BUTTERWORTH_Q_LOW, 0.0);
        self.sidechain[3].set_params(low, BUTTERWORTH_Q_HIGH, 0.0);
        self.sidechain[4].set_params(center, RESONANCE_Q, boost);
        for filter in &mut self.sidechain {
            filter.reset();
        }
    }

    fn update_ballistics(&mut self) {
        let rate = self.sample_rate as f64 * 0.001;
        let attack_tau = (self.attack_ms / BALLISTIC_T63_PER_TAU).max(0.01);
        let release_tau = (self.release_ms / BALLISTIC_T63_PER_TAU).max(0.01);
        self.attack_coeff = (-1.0 / (attack_tau * rate)).exp();
        self.release_coeff = (-1.0 / (release_tau * rate)).exp();
    }

    fn update_program(&mut self) {
        let rate = self.sample_rate as f64 * 0.001;
        self.program_attack_coeff = (-1.0 / (RELATIVE_ATTACK_MS * rate)).exp();
        self.program_release_coeff = (-1.0 / (RELATIVE_RELEASE_MS * rate)).exp();
    }

    fn update_lookahead(&mut self) {
        let line = (self.sample_rate as f64 * LOOKAHEAD_MS * 0.001).round() as usize;
        let knob = (self.sample_rate as f64 * self.lookahead_ms * 0.001).round() as usize;
        self.lookahead_knob = knob.min(line);
        self.lookahead_len = if self.lookahead_ms > 0.0 {
            line.max(1)
        } else {
            0
        };
        let ring = self.lookahead_len + 1;
        if self.delay_l.len() != ring {
            self.delay_l = vec![0.0; ring];
            self.delay_r = vec![0.0; ring];
        } else {
            self.delay_l.fill(0.0);
            self.delay_r.fill(0.0);
        }
        self.delay_pos = 0;
        for filter in &mut self.sidechain {
            filter.reset();
        }
    }

    fn apply_learned_band(&mut self, _band: SibilanceBand) {
        self.update_sidechain();
        self.update_split(true);
    }

    fn update_split(&mut self, preserve_history: bool) {
        let half = design_split_fir_into(
            self.split_corner_hz(),
            self.sample_rate,
            &mut self.split_fir,
            &mut self.split_kernel,
        );
        let resized = half != self.split_half;
        self.split_half = half;
        self.split_len = SPLIT_DELAY + half + 1;
        if !preserve_history {
            self.reset_split();
        } else if resized {
            self.reset_split_filter();
        }
    }

    fn reset_split(&mut self) {
        for channel in &mut self.split_dry {
            channel.fill(0.0);
        }
        self.split_dry_pos = 0;
        self.reset_split_filter();
    }

    fn reset_split_filter(&mut self) {
        self.split_l.fill(0.0);
        self.split_r.fill(0.0);
        self.detect_split_l.fill(0.0);
        self.detect_split_r.fill(0.0);
        self.split_pos = 0;
        self.detect_split_pos = 0;
    }

    fn gain_db(&self, level_db: f64) -> f64 {
        let over = level_db - self.threshold_db;
        ((self.range_db / 36.0) * over).clamp(0.0, self.range_db)
    }

    fn relative_gain_db(&self, level_db: f64, program_db: f64) -> f64 {
        let over = level_db - (program_db + self.relative_threshold_db);
        ((self.range_db / 36.0) * over).clamp(0.0, self.range_db)
    }

    fn program_level_db(&mut self, left: f64, right: f64) -> f64 {
        let power = 0.5 * (left * left + right * right);
        let coefficient = if power > self.program {
            self.program_attack_coeff
        } else {
            self.program_release_coeff
        };
        self.program = coefficient * self.program + (1.0 - coefficient) * power;
        10.0 * self.program.max(SILENCE_POWER).log10()
    }

    fn sidechain_peak(&mut self, channel: usize, input: f64) -> f64 {
        let mut peak: f64 = 0.0;
        for step in 0..SIDECHAIN_OVERSAMPLE {
            let mut value = if step == 0 {
                input * SIDECHAIN_OVERSAMPLE as f64
            } else {
                0.0
            };
            for filter in &mut self.sidechain {
                value = filter.process_sample(channel, value);
            }
            peak = peak.max(value.abs());
        }
        peak
    }

    fn hold_peak(&mut self, channel: usize, value: f64) -> f64 {
        self.peak_hold[channel][self.peak_hold_pos] = value;
        self.peak_hold[channel].iter().copied().fold(0.0, f64::max)
    }

    fn smooth_gr(&mut self, channel: usize, target: f64) -> f64 {
        let mut value = target;
        for stage in 0..BALLISTIC_POLES {
            let state = &mut self.ballistic[channel][stage];
            let coefficient = if value > *state {
                self.attack_coeff
            } else {
                self.release_coeff
            };
            *state = coefficient * *state + (1.0 - coefficient) * value;
            value = *state;
        }
        value
    }

    fn split_output(&mut self, channel: usize, audio: f64, gain: f64) -> f64 {
        let len = self.split_len;
        let buffer = if channel == 0 {
            &mut self.split_l
        } else {
            &mut self.split_r
        };
        buffer[self.split_pos] = audio;
        buffer[self.split_pos + len] = audio;
        let next = (self.split_pos + 1) % len;
        // The fixed dry delay survives learned FIR-length changes. Only spectral
        // history depends on the selected kernel length.
        let low = self.split_dry[channel][self.split_dry_pos];
        self.split_dry[channel][self.split_dry_pos] = audio;
        if gain == 1.0 {
            return low;
        }
        let span = 2 * self.split_half + 1;
        let high: f64 = self
            .split_fir
            .iter()
            .zip(&buffer[next..next + span])
            .map(|(coefficient, sample)| coefficient * sample)
            .sum();
        low + (gain - 1.0) * high
    }

    fn detect_split_high(&mut self, channel: usize, audio: f64) -> f64 {
        let len = self.split_len;
        let buffer = if channel == 0 {
            &mut self.detect_split_l
        } else {
            &mut self.detect_split_r
        };
        buffer[self.detect_split_pos] = audio;
        buffer[self.detect_split_pos + len] = audio;
        let next = (self.detect_split_pos + 1) % len;
        let span = 2 * self.split_half + 1;
        self.split_fir
            .iter()
            .zip(&buffer[next..next + span])
            .map(|(coefficient, sample)| coefficient * sample)
            .sum()
    }
}

impl AudioEffect for DeEsser {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let delay_len = self.delay_l.len();
        let detector_delay = if self.lookahead_len > 0 {
            self.lookahead_len - self.lookahead_knob
        } else {
            0
        };
        let mut frame = 0;
        while frame + 1 < buffer.len() {
            let input_l = buffer[frame] as f64;
            let input_r = buffer[frame + 1] as f64;

            let write = self.delay_pos;
            self.delay_l[write] = input_l as f32;
            self.delay_r[write] = input_r as f32;
            self.delay_pos = (write + 1) % delay_len;
            let (audio_l, audio_r) = if self.lookahead_len > 0 {
                let read = (write + delay_len - self.lookahead_len) % delay_len;
                (self.delay_l[read] as f64, self.delay_r[read] as f64)
            } else {
                (input_l, input_r)
            };
            let read = (write + delay_len - detector_delay) % delay_len;
            let detect_l = self.delay_l[read] as f64;
            let detect_r = self.delay_r[read] as f64;

            let estimate = self.band_estimation && self.band_override_hz <= 0.0;
            let detect = self.segment_detection;
            if estimate || detect {
                let (band_hz, top_hz) = self.effective_corners();
                let mid = 0.5 * (detect_l + detect_r);
                if let Some(band) =
                    self.analyzer
                        .push(mid, self.sample_rate, band_hz, top_hz, estimate, detect)
                {
                    self.apply_learned_band(band);
                }
            }
            if detect {
                let target = if self.analyzer.active() { 1.0 } else { 0.0 };
                let coefficient = if target > self.segment_gate {
                    self.segment_attack_coeff
                } else {
                    self.segment_release_coeff
                };
                self.segment_gate = coefficient * self.segment_gate + (1.0 - coefficient) * target;
            }

            let (peak_l, peak_r) = if self.in_band {
                if self.split_band {
                    let left = self.detect_split_high(0, detect_l);
                    let right = self.detect_split_high(1, detect_r);
                    self.detect_split_pos = (self.detect_split_pos + 1) % self.split_len;
                    (left.abs(), right.abs())
                } else {
                    (detect_l.abs(), detect_r.abs())
                }
            } else {
                (
                    self.sidechain_peak(0, detect_l),
                    self.sidechain_peak(1, detect_r),
                )
            };
            let level_l = self.hold_peak(0, peak_l);
            let level_r = self.hold_peak(1, peak_r);

            let (key_l, key_r) = if self.stereo_link > 0.0 {
                let mid = 0.5 * (level_l + level_r);
                (
                    level_l + self.stereo_link * (mid - level_l),
                    level_r + self.stereo_link * (mid - level_r),
                )
            } else {
                (level_l, level_r)
            };
            let program_db = if self.relative {
                self.program_level_db(detect_l, detect_r)
            } else {
                0.0
            };
            let pending = self.band_estimation
                && self.band_override_hz <= 0.0
                && self.analyzer.held().is_none();
            let target_l = if pending {
                0.0
            } else if self.relative {
                self.relative_gain_db(linear_to_db(key_l), program_db)
            } else {
                self.gain_db(linear_to_db(key_l))
            };
            let target_r = if pending {
                0.0
            } else if self.relative {
                self.relative_gain_db(linear_to_db(key_r), program_db)
            } else {
                self.gain_db(linear_to_db(key_r))
            };
            let (target_l, target_r) = if detect {
                (target_l * self.segment_gate, target_r * self.segment_gate)
            } else {
                (target_l, target_r)
            };
            let gr_l = self.smooth_gr(0, target_l);
            let gr_r = self.smooth_gr(1, target_r);
            self.last_gr_db = gr_l.max(gr_r);
            let gain_l = db_to_linear(-gr_l);
            let gain_r = db_to_linear(-gr_r);

            let (output_l, output_r) = if self.split_band {
                let left = self.split_output(0, audio_l, gain_l);
                let right = self.split_output(1, audio_r, gain_r);
                self.split_pos = (self.split_pos + 1) % self.split_len;
                self.split_dry_pos = (self.split_dry_pos + 1) % SPLIT_DELAY;
                (left, right)
            } else {
                (audio_l * gain_l, audio_r * gain_r)
            };
            buffer[frame] = output_l as f32;
            buffer[frame + 1] = output_r as f32;
            self.peak_hold_pos = (self.peak_hold_pos + 1) % self.peak_hold_len;
            frame += 2;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        match name {
            "threshold" => self.threshold_db = value.clamp(-60.0, 0.0),
            "threshold_mode" => self.relative = value >= 0.5,
            "relative_threshold" => self.relative_threshold_db = value.clamp(-36.0, 0.0),
            "range" => self.range_db = value.clamp(0.0, 24.0),
            "attack" => {
                self.attack_ms = value.clamp(0.1, 50.0);
                self.update_ballistics();
            }
            "release" => {
                self.release_ms = value.clamp(5.0, 1_000.0);
                self.update_ballistics();
            }
            "high_frequency" => {
                self.high_frequency_hz = value.clamp(2_000.0, 20_000.0);
                self.update_sidechain();
                self.update_split(false);
            }
            "low_frequency" => {
                self.low_frequency_hz = value.clamp(2_000.0, 20_000.0);
                self.update_sidechain();
            }
            "sibilance_band" => {
                let band = value.round().clamp(0.0, 5.0) as usize;
                if band != self.sibilance_band {
                    self.sibilance_band = band;
                    self.update_sidechain();
                    self.update_split(false);
                }
            }
            "band_estimation" => {
                let learn = value >= 0.5;
                if learn != self.band_estimation {
                    self.band_estimation = learn;
                    self.analyzer.reset();
                    self.update_sidechain();
                    self.update_split(false);
                }
            }
            "band_override" => {
                let band = value.clamp(0.0, 20_000.0);
                if band != self.band_override_hz {
                    self.band_override_hz = band;
                    self.update_sidechain();
                    self.update_split(false);
                }
            }
            "segment_detection" => {
                let detect = value >= 0.5;
                if detect != self.segment_detection {
                    self.segment_detection = detect;
                    self.segment_gate = 0.0;
                    self.analyzer.reset_segments();
                }
            }
            "band_processing" => {
                let split = value >= 0.5;
                if split != self.split_band {
                    self.split_band = split;
                    self.reset_split();
                }
            }
            "detector" => {
                let in_band = value >= 0.5;
                if in_band != self.in_band {
                    self.in_band = in_band;
                    self.reset_split();
                }
            }
            "stereo_link" => self.stereo_link = (value * 0.01).clamp(0.0, 1.0),
            "lookahead" => {
                self.lookahead_ms = value.clamp(0.0, MAX_LOOKAHEAD_MS);
                self.update_lookahead();
            }
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "threshold" => Some(self.threshold_db),
            "threshold_mode" => Some(if self.relative { 1.0 } else { 0.0 }),
            "relative_threshold" => Some(self.relative_threshold_db),
            "range" => Some(self.range_db),
            "attack" => Some(self.attack_ms),
            "release" => Some(self.release_ms),
            "high_frequency" => Some(self.high_frequency_hz),
            "low_frequency" => Some(self.low_frequency_hz),
            "sibilance_band" => Some(self.sibilance_band as f64),
            "band_processing" => Some(if self.split_band { 1.0 } else { 0.0 }),
            "detector" => Some(if self.in_band { 1.0 } else { 0.0 }),
            "stereo_link" => Some(self.stereo_link * 100.0),
            "lookahead" => Some(self.lookahead_ms),
            "band_estimation" => Some(if self.band_estimation { 1.0 } else { 0.0 }),
            "band_override" => Some(self.band_override_hz),
            "segment_detection" => Some(if self.segment_detection { 1.0 } else { 0.0 }),
            "chosen_band" => Some(self.effective_corners().0),
            "detected_band" => Some(self.detected_band_hz().unwrap_or(0.0)),
            "estimated_peak" => Some(self.estimated_peak_hz().unwrap_or(0.0)),
            _ => None,
        }
    }

    fn reset(&mut self) {
        for filter in &mut self.sidechain {
            filter.reset();
        }
        self.peak_hold[0].fill(0.0);
        self.peak_hold[1].fill(0.0);
        self.peak_hold_pos = 0;
        self.ballistic = [[0.0; BALLISTIC_POLES]; 2];
        self.program = 0.0;
        self.delay_l.fill(0.0);
        self.delay_r.fill(0.0);
        self.delay_pos = 0;
        self.reset_split();
        self.analyzer.reset();
        self.segment_gate = 0.0;
        self.update_sidechain();
        self.update_split(false);
        self.last_gr_db = 0.0;
    }

    fn latency_samples(&self) -> u32 {
        let split = if self.split_band {
            SPLIT_DELAY as u32
        } else {
            0
        };
        self.lookahead_len as u32 + split
    }

    fn tail_samples(&self) -> u32 {
        let split = if self.split_band {
            self.split_half as u32
        } else {
            0
        };
        self.latency_samples() + split
    }

    fn gain_reduction_db(&self) -> f64 {
        self.last_gr_db
    }

    fn name(&self) -> &str {
        "De-esser"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biquad::BiquadCoefficients;

    const SR: u32 = 48_000;

    fn sine(freq: f64, amplitude: f64, frames: usize) -> Vec<f32> {
        let mut buffer = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let value = (std::f64::consts::TAU * freq * frame as f64 / SR as f64).sin() * amplitude;
            buffer[frame * 2] = value as f32;
            buffer[frame * 2 + 1] = value as f32;
        }
        buffer
    }

    fn add_tone(buffer: &mut [f32], freq: f64, amplitude: f64) {
        for frame in 0..buffer.len() / 2 {
            let value = (std::f64::consts::TAU * freq * frame as f64 / SR as f64).sin() * amplitude;
            buffer[frame * 2] += value as f32;
            buffer[frame * 2 + 1] += value as f32;
        }
    }

    fn rms_window(buffer: &[f32], start_frame: usize, frames: usize) -> f64 {
        let end = (start_frame + frames).min(buffer.len() / 2);
        let samples: Vec<f64> = (start_frame..end)
            .map(|frame| (buffer[frame * 2] as f64).powi(2))
            .collect();
        (samples.iter().sum::<f64>() / samples.len().max(1) as f64).sqrt()
    }

    fn delayed(buffer: &[f32], delay: usize) -> Vec<f32> {
        let frames = buffer.len() / 2;
        let mut output = vec![0.0f32; buffer.len()];
        for frame in delay..frames {
            output[frame * 2] = buffer[(frame - delay) * 2];
            output[frame * 2 + 1] = buffer[(frame - delay) * 2 + 1];
        }
        output
    }

    fn amplitude(buffer: &[f32], freq: f64, start_frame: usize, channel: usize) -> f64 {
        let frames = buffer.len() / 2 - start_frame;
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for frame in start_frame..buffer.len() / 2 {
            let value = buffer[frame * 2 + channel] as f64;
            let phase = std::f64::consts::TAU * freq * frame as f64 / SR as f64;
            real += value * phase.cos();
            imaginary += value * phase.sin();
        }
        2.0 * (real * real + imaginary * imaginary).sqrt() / frames as f64
    }

    fn tone_gain_db(
        output: &[f32],
        input: &[f32],
        freq: f64,
        start_frame: usize,
        channel: usize,
    ) -> f64 {
        20.0 * (amplitude(output, freq, start_frame, channel)
            / amplitude(input, freq, start_frame, channel))
        .log10()
    }

    fn phase_deg(buffer: &[f32], freq: f64, start_frame: usize, channel: usize) -> f64 {
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for frame in start_frame..buffer.len() / 2 {
            let value = buffer[frame * 2 + channel] as f64;
            let phase = std::f64::consts::TAU * freq * frame as f64 / SR as f64;
            real += value * phase.cos();
            imaginary += value * phase.sin();
        }
        (imaginary.atan2(real) * 180.0 / std::f64::consts::PI).rem_euclid(360.0)
    }

    fn sidechain_db(freq: f64) -> f64 {
        let rate = SR as f64 * SIDECHAIN_OVERSAMPLE as f64;
        let high: f64 = 7_500.0;
        let low: f64 = 14_000.0;
        let center = (high * low).sqrt();
        let boost = band_boost_db(low / high);
        let mut db = 0.0;
        for (kind, corner, q) in [
            (FilterType::HighPass, high, BUTTERWORTH_Q_LOW),
            (FilterType::HighPass, high, BUTTERWORTH_Q_HIGH),
            (FilterType::LowPass, low, BUTTERWORTH_Q_LOW),
            (FilterType::LowPass, low, BUTTERWORTH_Q_HIGH),
            (FilterType::Peak, center, RESONANCE_Q),
        ] {
            let gain = if kind == FilterType::Peak { boost } else { 0.0 };
            db += BiquadCoefficients::design(kind, corner, q, gain, rate).response_db(freq, rate);
        }
        db
    }

    #[test]
    fn gain_law_is_a_hard_threshold_whose_range_is_slope_and_ceiling() {
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold", -36.0);
        de_esser.set_param("range", 6.0);
        assert_eq!(de_esser.gain_db(-36.0), 0.0);
        assert_eq!(de_esser.gain_db(-40.0), 0.0);
        assert!((de_esser.gain_db(-35.0) - 6.0 / 36.0).abs() < 1e-12);
        assert!((de_esser.gain_db(-33.0) - 0.5).abs() < 1e-12);
        assert!((de_esser.gain_db(0.0) - 6.0).abs() < 1e-12);

        de_esser.set_param("range", 24.0);
        assert!((de_esser.gain_db(-33.0) - 2.0).abs() < 1e-12);
        assert!((de_esser.gain_db(-30.0) - 4.0).abs() < 1e-12);
        assert!((de_esser.gain_db(0.0) - 24.0).abs() < 1e-12);
        assert_eq!(de_esser.gain_db(-36.5), 0.0);

        de_esser.set_param("threshold", -48.0);
        assert_eq!(de_esser.gain_db(-48.0), 0.0);
        assert!((de_esser.gain_db(-42.0) - 4.0).abs() < 1e-12);
    }

    #[test]
    fn sidechain_band_is_24_db_per_octave_with_the_measured_in_band_gain() {
        assert!((sidechain_db(10_000.0) - 6.96).abs() < 0.2);
        assert!((sidechain_db(8_000.0) - 3.93).abs() < 0.2);
        assert!((sidechain_db(6_000.0) - -5.03).abs() < 0.2);
        assert!((sidechain_db(2_000.0) - -45.6).abs() < 0.5);
        assert!((sidechain_db(500.0) - -94.0).abs() < 0.5);
        let low_slope = sidechain_db(500.0) - sidechain_db(1_000.0);
        assert!((-low_slope - 24.1).abs() < 1.0, "{low_slope} dB/oct");
    }

    #[test]
    fn sidechain_drives_the_static_gain_reduction_law() {
        let input = sine(10_000.0, 10.0_f64.powf(-40.0 / 20.0), SR as usize);
        let mut output = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold", -36.0);
        de_esser.set_param("range", 24.0);
        de_esser.process(&mut output, SR);
        let gr = -tone_gain_db(&output, &input, 10_000.0, 24_000, 0);
        let expected = (24.0 / 36.0) * (-40.0 + 6.96 + 36.0);
        assert!((gr - expected).abs() < 0.1, "GR {gr} expected {expected}");
    }

    #[test]
    fn below_threshold_wide_band_is_bit_identical() {
        let input = sine(8_000.0, 0.2, 4_800);
        let mut output = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold", 0.0);
        de_esser.set_param("range", 24.0);
        de_esser.process(&mut output, SR);
        assert_eq!(output, input);
        assert_eq!(de_esser.gain_reduction_db(), 0.0);
    }

    #[test]
    fn split_band_neutral_is_an_exact_512_sample_delay() {
        let mut input = sine(1_000.0, 0.2, 4_800);
        add_tone(&mut input, 8_000.0, 0.1);
        let mut output = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold", 0.0);
        de_esser.set_param("band_processing", 1.0);
        de_esser.process(&mut output, SR);
        assert_eq!(de_esser.latency_samples(), SPLIT_DELAY as u32);
        let frames = input.len() / 2;
        let mut max_difference = 0.0f32;
        for frame in 0..frames - SPLIT_DELAY {
            for channel in 0..2 {
                let difference = (output[(frame + SPLIT_DELAY) * 2 + channel]
                    - input[frame * 2 + channel])
                    .abs();
                max_difference = max_difference.max(difference);
            }
        }
        assert_eq!(max_difference, 0.0);
    }

    #[test]
    fn neutral_split_delay_survives_learned_filter_length_changes() {
        let mut input = sine(1_000.0, 0.2, 4_800);
        add_tone(&mut input, 8_000.0, 0.1);
        input[123] = -0.0;
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("band_processing", 1.0);
        de_esser.set_param("range", 0.0);
        let mut output = input.clone();
        output.resize(input.len() + SPLIT_DELAY * 2, 0.0);
        let retune_at = 1_700 * 2;
        de_esser.process(&mut output[..retune_at], SR);
        let previous_half = de_esser.split_half;
        de_esser.band_override_hz = 8_000.0;
        de_esser.update_split(true);
        assert_ne!(de_esser.split_half, previous_half, "retune resizes the FIR");
        assert!(de_esser.split_l.iter().all(|sample| *sample == 0.0));
        de_esser.process(&mut output[retune_at..], SR);
        for (sample, value) in output[..SPLIT_DELAY * 2].iter().enumerate() {
            assert_eq!(value.to_bits(), 0.0f32.to_bits(), "startup sample {sample}");
        }
        for (sample, (actual, expected)) in output[SPLIT_DELAY * 2..].iter().zip(&input).enumerate()
        {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "retuned PCM sample {sample}"
            );
        }
        de_esser.reset();
        let mut silence = vec![0.0; SPLIT_DELAY * 2];
        de_esser.process(&mut silence, SR);
        assert!(silence.iter().all(|sample| sample.to_bits() == 0));
    }

    #[test]
    fn split_recombination_is_zero_phase_power_complementary() {
        let frames = 48_000;
        let mut input = sine(1_000.0, 0.2, frames);
        add_tone(&mut input, 8_000.0, 0.2);
        let mut wide = input.clone();
        let mut split = input.clone();
        let mut wide_fx = DeEsser::new(SR);
        wide_fx.set_param("threshold", -48.0);
        wide_fx.set_param("range", 24.0);
        wide_fx.process(&mut wide, SR);
        let mut split_fx = DeEsser::new(SR);
        split_fx.set_param("threshold", -48.0);
        split_fx.set_param("range", 24.0);
        split_fx.set_param("band_processing", 1.0);
        split_fx.process(&mut split, SR);

        let wide_hf = tone_gain_db(&wide, &input, 8_000.0, 24_000, 0);
        let gain = 10.0_f64.powf(wide_hf / 20.0);
        let h_split = |freq: f64| {
            let x = freq / (7_500.0 / 4.0);
            x * x / (1.0 + x * x)
        };
        let hf_split = h_split(8_000.0);
        let lf_split = h_split(1_000.0);
        let expected_hf = 20.0 * (1.0 + (gain - 1.0) * hf_split).log10();
        let expected_lf = 20.0 * (1.0 + (gain - 1.0) * lf_split).log10();
        let split_hf = tone_gain_db(&split, &input, 8_000.0, 24_000, 0);
        let split_lf = tone_gain_db(&split, &input, 1_000.0, 24_000, 0);
        assert!(
            (split_hf - expected_hf).abs() < 0.3,
            "{split_hf} vs {expected_hf}"
        );
        assert!(
            (split_lf - expected_lf).abs() < 0.3,
            "{split_lf} vs {expected_lf}"
        );

        let reference = delayed(&input, SPLIT_DELAY);
        for freq in [1_000.0, 8_000.0] {
            let output_phase = phase_deg(&split, freq, 24_000, 0);
            let reference_phase = phase_deg(&reference, freq, 24_000, 0);
            let error = (output_phase - reference_phase + 540.0).rem_euclid(360.0) - 180.0;
            assert!(error.abs() < 5.0, "{freq} Hz phase error {error}");
        }
    }

    #[test]
    fn stereo_link_blends_to_the_mid_at_full_link() {
        let frames = 48_000;
        let mut input = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let left = (std::f64::consts::TAU * 8_000.0 * frame as f64 / SR as f64).sin() * 0.25;
            let right = (std::f64::consts::TAU * 8_000.0 * frame as f64 / SR as f64).sin() * 0.001;
            input[frame * 2] = left as f32;
            input[frame * 2 + 1] = right as f32;
        }
        for (link, expected_left, expected_right) in
            [(0.0, 18.6, 0.0), (50.0, 16.9, 10.6), (100.0, 14.6, 14.6)]
        {
            let mut output = input.clone();
            let mut de_esser = DeEsser::new(SR);
            de_esser.set_param("threshold", -36.0);
            de_esser.set_param("range", 24.0);
            de_esser.set_param("stereo_link", link);
            de_esser.process(&mut output, SR);
            let left = -tone_gain_db(&output, &input, 8_000.0, 24_000, 0);
            let right = -tone_gain_db(&output, &input, 8_000.0, 24_000, 1);
            assert!(
                (left - expected_left).abs() < 0.35,
                "link {link} left {left} expected {expected_left}"
            );
            assert!(
                (right - expected_right).abs() < 0.35,
                "link {link} right {right} expected {expected_right}"
            );
        }
    }

    #[test]
    fn ballistics_use_11_ms_attack_and_15_ms_release() {
        let frames = SR as usize;
        let mut input = sine(8_000.0, 0.001, frames);
        let step = frames / 2;
        for frame in step..frames {
            let value = (std::f64::consts::TAU * 8_000.0 * frame as f64 / SR as f64).sin() * 0.25;
            input[frame * 2] = value as f32;
            input[frame * 2 + 1] = value as f32;
        }
        let mut output = input.clone();
        let mut reference = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold", -36.0);
        de_esser.set_param("range", 24.0);
        de_esser.process(&mut output, SR);
        let mut reference_fx = DeEsser::new(SR);
        reference_fx.set_param("threshold", 0.0);
        reference_fx.set_param("range", 24.0);
        reference_fx.process(&mut reference, SR);
        let final_gr = 20.0
            * (rms_window(&reference, frames - 2_400, 2_400)
                / rms_window(&output, frames - 2_400, 2_400))
            .log10();
        let mut t10 = None;
        let mut t63 = None;
        for offset in 1..2_400 {
            let gain = 20.0
                * (rms_window(&reference, step + offset, 24)
                    / rms_window(&output, step + offset, 24))
                .log10();
            if gain / final_gr >= 0.1 && t10.is_none() {
                t10 = Some(offset as f64 / 48.0);
            }
            if gain / final_gr >= 0.63 && t63.is_none() {
                t63 = Some(offset as f64 / 48.0);
            }
        }
        assert!(t10.unwrap() > 2.0 && t10.unwrap() < 8.0, "{t10:?}");
        assert!((t63.unwrap() - 11.0).abs() < 2.0, "{t63:?}");
        assert_eq!(de_esser.get_param("attack"), Some(11.0));
        assert_eq!(de_esser.get_param("release"), Some(15.5));
    }

    #[test]
    fn lookahead_is_a_fixed_15_ms_line() {
        for lookahead in [3.0, 15.0] {
            let mut de_esser = DeEsser::new(SR);
            de_esser.set_param("threshold", 0.0);
            de_esser.set_param("lookahead", lookahead);
            assert_eq!(de_esser.latency_samples(), 720);
            let mut signal = vec![0.0f32; 16_384];
            signal[2_000] = 0.5;
            signal[2_001] = 0.5;
            de_esser.process(&mut signal, SR);
            let onset = signal
                .chunks_exact(2)
                .position(|frame| frame[0].abs() > 1e-9)
                .expect("delayed impulse");
            assert_eq!(onset, 1_000 + 720);
        }

        let mut split = DeEsser::new(SR);
        split.set_param("threshold", 0.0);
        split.set_param("lookahead", 3.0);
        split.set_param("band_processing", 1.0);
        assert_eq!(split.latency_samples(), 1_232);
        let mut signal = vec![0.0f32; 16_384];
        signal[2_000] = 0.5;
        signal[2_001] = 0.5;
        split.process(&mut signal, SR);
        let onset = signal
            .chunks_exact(2)
            .position(|frame| frame[0].abs() > 1e-9)
            .expect("delayed impulse");
        assert_eq!(onset, 1_000 + 1_232);

        let mut wide = DeEsser::new(SR);
        wide.set_param("threshold", 0.0);
        assert_eq!(wide.latency_samples(), 0);
    }

    #[test]
    fn split_is_not_a_subtractive_bandpass() {
        // The split uses a zero-phase high band delayed by 512 samples.
        let frames = 24_000;
        let mut input = vec![0.0f32; frames * 2];
        input[2_000] = 0.5;
        input[2_001] = 0.5;
        let mut output = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold", 0.0);
        de_esser.set_param("band_processing", 1.0);
        de_esser.process(&mut output, SR);
        let onset = output
            .chunks_exact(2)
            .position(|frame| frame[0].abs() > 1e-9)
            .expect("delayed output");
        assert_eq!(onset, 1_000 + SPLIT_DELAY);
    }

    #[test]
    fn split_high_band_matches_the_power_complementary_law() {
        let (fir, half) = design_split_fir(7_500.0 / 4.0, SR);
        for (freq, target) in [
            (500.0, 0.067),
            (1_000.0, 0.222),
            (2_000.0, 0.532),
            (8_000.0, 0.954),
        ] {
            let mut real = 0.0;
            let mut imaginary = 0.0;
            for (index, tap) in fir.iter().enumerate() {
                let delay = SPLIT_DELAY as f64 - half as f64 + index as f64;
                let phase = std::f64::consts::TAU * freq * delay / SR as f64;
                real += tap * phase.cos();
                imaginary -= tap * phase.sin();
            }
            let magnitude = (real * real + imaginary * imaginary).sqrt();
            assert!(
                (magnitude - target).abs() < 0.01,
                "{freq} Hz: {magnitude} vs {target}"
            );
        }
    }

    #[test]
    fn extreme_settings_stay_finite_and_bounded() {
        let input = sine(9_000.0, 1.0, 24_000);
        for split in [0.0, 1.0] {
            for lookahead in [0.0, 15.0] {
                let mut output = input.clone();
                let mut de_esser = DeEsser::new(SR);
                de_esser.set_param("threshold", -60.0);
                de_esser.set_param("range", 24.0);
                de_esser.set_param("attack", 0.1);
                de_esser.set_param("release", 5.0);
                de_esser.set_param("high_frequency", 2_000.0);
                de_esser.set_param("low_frequency", 20_000.0);
                de_esser.set_param("stereo_link", 0.0);
                de_esser.set_param("band_processing", split);
                de_esser.set_param("lookahead", lookahead);
                de_esser.process(&mut output, SR);
                assert!(output.iter().all(|value| value.is_finite()));
                assert!(output.iter().all(|value| value.abs() <= 1.5));
            }
        }
    }

    #[test]
    fn analog_modes_add_no_latency() {
        for (params, expected) in [
            (vec![("threshold_mode", 1.0), ("band_processing", 1.0)], 512),
            (vec![("detector", 1.0), ("band_processing", 1.0)], 512),
            (vec![("detector", 1.0)], 0),
            (
                vec![
                    ("threshold_mode", 1.0),
                    ("detector", 1.0),
                    ("band_processing", 1.0),
                    ("sibilance_band", 5.0),
                    ("lookahead", 15.0),
                ],
                1_232,
            ),
        ] {
            let mut de_esser = DeEsser::new(SR);
            de_esser.set_param("threshold", 0.0);
            for &(name, value) in &params {
                de_esser.set_param(name, value);
            }
            assert_eq!(de_esser.latency_samples(), expected, "{params:?}");
        }
    }

    #[test]
    fn reset_is_deterministic() {
        let input = sine(8_000.0, 0.3, 9_600);
        let mut first = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold", -48.0);
        de_esser.set_param("range", 24.0);
        de_esser.set_param("band_processing", 1.0);
        de_esser.process(&mut first, SR);
        de_esser.reset();
        let mut second = input.clone();
        de_esser.process(&mut second, SR);
        assert_eq!(first, second);
    }

    fn window_amplitude(buffer: &[f32], freq: f64, start: usize, frames: usize) -> f64 {
        let end = (start + frames).min(buffer.len() / 2);
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for frame in start..end {
            let value = buffer[frame * 2] as f64;
            let phase = std::f64::consts::TAU * freq * frame as f64 / SR as f64;
            real += value * phase.cos();
            imaginary += value * phase.sin();
        }
        2.0 * (real * real + imaginary * imaginary).sqrt() / (end - start).max(1) as f64
    }

    fn golden_program() -> Vec<f32> {
        let frames = SR as usize * 3;
        let mut buffer = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let t = frame as f64 / SR as f64;
            let body = (std::f64::consts::TAU * 1_000.0 * t).sin() * 0.2;
            let pulse = 0.5 + 0.5 * (std::f64::consts::TAU * 2.0 * t).sin();
            let sib = (std::f64::consts::TAU * 7_000.0 * t).sin() * 0.12 * pulse;
            buffer[frame * 2] = (body + sib) as f32;
            buffer[frame * 2 + 1] = (body + sib * 0.8) as f32;
        }
        buffer
    }

    fn hash_output(buffer: &[f32]) -> u64 {
        let mut value: u64 = 0xcbf2_9ce4_8422_2325;
        for sample in buffer {
            value ^= sample.to_bits() as u64;
            value = value.wrapping_mul(0x100_0000_01b3);
        }
        value
    }

    #[test]
    fn defaults_render_bit_identically_to_the_shipped_device() {
        struct GoldenCase {
            label: &'static str,
            params: &'static [(&'static str, f64)],
            expected: u64,
        }
        let cases = [
            GoldenCase {
                label: "wide defaults",
                params: &[],
                expected: 0x1493_6a92_bc4f_0075,
            },
            GoldenCase {
                label: "split band",
                params: &[("band_processing", 1.0)],
                expected: crate::golden_references::select(
                    0x495e_f0f3_cc1a_d463,
                    0x6a7e_629a_8d27_3eb9,
                    0x630c_6fd6_06f2_74fb,
                ),
            },
            GoldenCase {
                label: "lookahead and link",
                params: &[("lookahead", 15.0), ("stereo_link", 50.0)],
                expected: 0x4bf8_92bc_f56d_89f4,
            },
            GoldenCase {
                label: "split 6 kHz",
                params: &[
                    ("band_processing", 1.0),
                    ("high_frequency", 6_000.0),
                    ("low_frequency", 12_000.0),
                    ("threshold", -40.0),
                ],
                expected: crate::golden_references::select(
                    0x4a5b_8c66_7bd5_18fc,
                    0x6000_d3d5_8afb_aa9b,
                    0xc4d5_a87a_005a_d6e0,
                ),
            },
        ];
        let mut actual_hashes = Vec::new();
        let mut expected_hashes = Vec::new();
        for GoldenCase {
            label,
            params,
            expected,
        } in cases
        {
            let mut de_esser = DeEsser::new(SR);
            for &(name, value) in params {
                de_esser.set_param(name, value);
            }
            let mut buffer = golden_program();
            let input_hash = hash_output(&buffer);
            assert_eq!(
                input_hash, 0xc7cb_4d78_7597_5dbc,
                "DeEsser golden input: {label}"
            );
            de_esser.process(&mut buffer, SR);
            let actual = hash_output(&buffer);
            let (power, max_abs) =
                buffer
                    .iter()
                    .fold((0.0_f64, 0.0_f64), |(power, max_abs), &sample| {
                        let value = f64::from(sample);
                        (power + value * value, max_abs.max(value.abs()))
                    });
            let rms = (power / buffer.len() as f64).sqrt();
            let nonfinite = buffer.iter().filter(|sample| !sample.is_finite()).count();
            eprintln!(
                "deesser golden case={label:?} samples={} input_hash={input_hash:016x} output_hash={actual:016x} expected={expected:016x} output_rms={rms:.12e} output_max_abs={max_abs:.12e} nonfinite={nonfinite}",
                buffer.len()
            );
            actual_hashes.push(actual);
            expected_hashes.push(expected);
        }
        assert_eq!(
            actual_hashes, expected_hashes,
            "all DeEsser golden cases in declared order"
        );
    }

    #[test]
    fn new_mode_defaults_are_no_ops() {
        let mut reference = golden_program();
        DeEsser::new(SR).process(&mut reference, SR);
        let mut output = golden_program();
        let mut de_esser = DeEsser::new(SR);
        for (name, value) in [
            ("threshold_mode", 0.0),
            ("detector", 0.0),
            ("sibilance_band", 0.0),
            ("relative_threshold", -12.0),
            ("band_estimation", 0.0),
            ("band_override", 0.0),
            ("segment_detection", 0.0),
        ] {
            de_esser.set_param(name, value);
        }
        de_esser.process(&mut output, SR);
        assert_eq!(output, reference);
    }

    #[test]
    fn relative_gain_law_tracks_the_program_at_one_to_one() {
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("range", 24.0);
        de_esser.set_param("relative_threshold", -12.0);
        assert!((de_esser.relative_gain_db(-32.0, -20.0) - 0.0).abs() < 1e-12);
        assert!((de_esser.relative_gain_db(-34.0, -20.0) - 0.0).abs() < 1e-12);
        assert!((de_esser.relative_gain_db(-20.0, -20.0) - 8.0).abs() < 1e-12);
        assert!((de_esser.relative_gain_db(-14.0, -14.0) - 8.0).abs() < 1e-12);
        assert!((de_esser.relative_gain_db(-30.0, -20.0) - 4.0 / 3.0).abs() < 1e-12);
        assert!((de_esser.relative_gain_db(0.0, 0.0) - 8.0).abs() < 1e-12);
    }

    #[test]
    fn relative_program_window_is_200_ms_attack_and_1_s_release() {
        let mut de_esser = DeEsser::new(SR);
        let mut level = 0.0;
        for _ in 0..(SR as usize * 200 / 1_000) {
            level = de_esser.program_level_db(1.0, 1.0);
        }
        assert!((level + 1.99).abs() < 0.1, "attack T63 {level}");
        de_esser.program = 1.0;
        for _ in 0..(SR as usize) {
            level = de_esser.program_level_db(0.0, 0.0);
        }
        assert!((level + 4.34).abs() < 0.1, "release T63 {level}");
    }

    #[test]
    fn relative_threshold_is_level_independent() {
        let measure = |scale: f64| {
            let frames = SR as usize * 2;
            let mut input = vec![0.0f32; frames * 2];
            for frame in 0..frames {
                let t = frame as f64 / SR as f64;
                let body = (std::f64::consts::TAU * 1_000.0 * t).sin() * 0.2 * scale;
                let sib = (std::f64::consts::TAU * 8_000.0 * t).sin() * 0.05 * scale;
                input[frame * 2] = (body + sib) as f32;
                input[frame * 2 + 1] = (body + sib) as f32;
            }
            let mut output = input.clone();
            let mut de_esser = DeEsser::new(SR);
            de_esser.set_param("threshold_mode", 1.0);
            de_esser.set_param("relative_threshold", -12.0);
            de_esser.set_param("range", 24.0);
            de_esser.set_param("band_processing", 1.0);
            de_esser.set_param("sibilance_band", 3.0);
            de_esser.process(&mut output, SR);
            -tone_gain_db(&output, &input, 8_000.0, SR as usize, 0)
        };
        let loud = measure(1.0);
        let quiet = measure(0.1);
        assert!(loud > 2.0, "loud reduction {loud}");
        assert!(
            (loud - quiet).abs() < 0.2,
            "loud {loud} dB, quiet {quiet} dB"
        );
    }

    #[test]
    fn relative_threshold_does_not_pump_on_a_crescendo() {
        let frames = SR as usize * 6;
        let mut input = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let t = frame as f64 / SR as f64;
            let db = -34.0 + 20.0 * t / 6.0;
            let amplitude = 10.0_f64.powf(db / 20.0);
            let body = (std::f64::consts::TAU * 1_000.0 * t).sin() * amplitude;
            let sib = (std::f64::consts::TAU * 8_000.0 * t).sin() * amplitude * 0.25;
            input[frame * 2] = (body + sib) as f32;
            input[frame * 2 + 1] = (body + sib) as f32;
        }
        let mut output = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold_mode", 1.0);
        de_esser.set_param("relative_threshold", -12.0);
        de_esser.set_param("range", 24.0);
        de_esser.set_param("band_processing", 1.0);
        de_esser.set_param("sibilance_band", 3.0);
        de_esser.process(&mut output, SR);
        let window = SR as usize;
        let body_gain = |start: usize| {
            20.0 * (window_amplitude(&output, 1_000.0, start, window)
                / window_amplitude(&input, 1_000.0, start, window))
            .log10()
        };
        let sibilance_gain = |start: usize| {
            20.0 * (window_amplitude(&output, 8_000.0, start, window)
                / window_amplitude(&input, 8_000.0, start, window))
            .log10()
        };
        let body_movement = (body_gain(5 * SR as usize) - body_gain(SR as usize)).abs();
        let sibilance_movement =
            (sibilance_gain(5 * SR as usize) - sibilance_gain(SR as usize)).abs();
        assert!(body_movement < 0.5, "body moved {body_movement} dB");
        assert!(
            sibilance_movement < 0.5,
            "sibilance moved {sibilance_movement} dB"
        );
    }

    #[test]
    fn full_band_ducks_the_body_by_the_same_gain_as_the_sibilance() {
        let frames = SR as usize * 2;
        let mut input = sine(1_000.0, 0.2, frames);
        add_tone(&mut input, 8_000.0, 0.05);
        let mut output = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold", -36.0);
        de_esser.set_param("range", 24.0);
        de_esser.process(&mut output, SR);
        let sibilance = -tone_gain_db(&output, &input, 8_000.0, SR as usize, 0);
        let body = -tone_gain_db(&output, &input, 1_000.0, SR as usize, 0);
        assert!(sibilance > 2.0, "sibilance reduction {sibilance}");
        assert!(
            (body - sibilance).abs() < 0.1,
            "body {body} dB vs sibilance {sibilance} dB"
        );
    }

    #[test]
    fn full_band_detection_lets_the_body_trigger_the_reduction() {
        let frames = SR as usize * 2;
        let input = sine(1_000.0, 0.1, frames);
        let mut output = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold", -36.0);
        de_esser.set_param("range", 24.0);
        de_esser.set_param("detector", 1.0);
        de_esser.process(&mut output, SR);
        let body = -tone_gain_db(&output, &input, 1_000.0, SR as usize, 0);
        assert!(body > 5.0, "body reduction {body}");
        assert!(
            (body - de_esser.gain_reduction_db()).abs() < 0.1,
            "body {body} dB vs GR {} dB",
            de_esser.gain_reduction_db()
        );
    }

    #[test]
    fn detector_placement_changes_which_signal_triggers_reduction() {
        let frames = SR as usize * 2;
        let input = sine(1_000.0, 0.1, frames);
        let mut sidechain = input.clone();
        let mut sidechain_fx = DeEsser::new(SR);
        sidechain_fx.set_param("threshold", -36.0);
        sidechain_fx.set_param("range", 24.0);
        sidechain_fx.process(&mut sidechain, SR);
        let mut in_band = input.clone();
        let mut in_band_fx = DeEsser::new(SR);
        in_band_fx.set_param("threshold", -36.0);
        in_band_fx.set_param("range", 24.0);
        in_band_fx.set_param("detector", 1.0);
        in_band_fx.process(&mut in_band, SR);
        assert_eq!(sidechain, input, "sidechain ignored the 1 kHz body");
        let body = -tone_gain_db(&in_band, &input, 1_000.0, SR as usize, 0);
        assert!(body > 5.0, "in-band body reduction {body}");

        let sibilant = sine(8_000.0, 0.05, frames);
        let mut sidechain = sibilant.clone();
        let mut sidechain_fx = DeEsser::new(SR);
        sidechain_fx.set_param("threshold", -36.0);
        sidechain_fx.set_param("range", 24.0);
        sidechain_fx.set_param("band_processing", 1.0);
        sidechain_fx.set_param("sibilance_band", 3.0);
        sidechain_fx.process(&mut sidechain, SR);
        let mut in_band = sibilant.clone();
        let mut in_band_fx = DeEsser::new(SR);
        in_band_fx.set_param("threshold", -36.0);
        in_band_fx.set_param("range", 24.0);
        in_band_fx.set_param("band_processing", 1.0);
        in_band_fx.set_param("sibilance_band", 3.0);
        in_band_fx.set_param("detector", 1.0);
        in_band_fx.process(&mut in_band, SR);
        let difference = (sidechain_fx.gain_reduction_db() - in_band_fx.gain_reduction_db()).abs();
        assert!(difference > 3.0, "placement moved GR by {difference} dB");
    }

    #[test]
    fn sibilance_band_steps_move_the_split_and_sidechain() {
        for (index, band) in [
            (1.0, 5_000.0),
            (2.0, 6_000.0),
            (3.0, 7_000.0),
            (4.0, 8_000.0),
            (5.0, 9_000.0),
        ] {
            let sibilance = band * 1.4;
            let frames = SR as usize * 2;
            let mut input = sine(1_000.0, 0.2, frames);
            add_tone(&mut input, sibilance, 0.05);
            let mut output = input.clone();
            let mut de_esser = DeEsser::new(SR);
            de_esser.set_param("threshold", -36.0);
            de_esser.set_param("range", 24.0);
            de_esser.set_param("band_processing", 1.0);
            de_esser.set_param("sibilance_band", index);
            de_esser.process(&mut output, SR);
            let body = -tone_gain_db(&output, &input, 1_000.0, SR as usize, 0);
            let sibilance_gain = -tone_gain_db(&output, &input, sibilance, SR as usize, 0);
            assert!(body < 0.5, "{band} Hz body {body} dB");
            assert!(
                sibilance_gain > 2.0,
                "{band} Hz sibilance {sibilance_gain} dB"
            );
            assert!(de_esser.gain_reduction_db() > 3.0);
        }
    }

    #[test]
    fn split_corner_is_the_selected_band_not_hp_over_4() {
        for band in [5_000.0, 6_000.0, 7_000.0, 8_000.0, 9_000.0] {
            let (fir, half) = design_split_fir(band, SR);
            let mut real = 0.0;
            let mut imaginary = 0.0;
            for (index, tap) in fir.iter().enumerate() {
                let delay = SPLIT_DELAY as f64 - half as f64 + index as f64;
                let phase = std::f64::consts::TAU * band * delay / SR as f64;
                real += tap * phase.cos();
                imaginary -= tap * phase.sin();
            }
            let magnitude = (real * real + imaginary * imaginary).sqrt();
            assert!(
                (magnitude - 0.5).abs() < 0.02,
                "{band} Hz split magnitude {magnitude}"
            );
        }
    }

    #[test]
    #[ignore]
    fn measurements_for_docs() {
        let frames = SR as usize * 2;
        for index in 1..=5 {
            let band = 4_000.0 + 1_000.0 * index as f64;
            let sibilance = band * 1.4;
            let mut input = sine(1_000.0, 0.2, frames);
            add_tone(&mut input, sibilance, 0.05);
            let mut output = input.clone();
            let mut de_esser = DeEsser::new(SR);
            de_esser.set_param("threshold", -36.0);
            de_esser.set_param("range", 24.0);
            de_esser.set_param("band_processing", 1.0);
            de_esser.set_param("sibilance_band", index as f64);
            de_esser.process(&mut output, SR);
            println!(
                "band {band:.0} Hz: GR {:.2} dB, sibilance {:.2} dB, body {:.2} dB",
                de_esser.gain_reduction_db(),
                -tone_gain_db(&output, &input, sibilance, SR as usize, 0),
                -tone_gain_db(&output, &input, 1_000.0, SR as usize, 0),
            );
        }

        let relative = |scale: f64| {
            let mut input = sine(1_000.0, 0.2 * scale, frames);
            add_tone(&mut input, 8_000.0, 0.05 * scale);
            let mut output = input.clone();
            let mut de_esser = DeEsser::new(SR);
            de_esser.set_param("threshold_mode", 1.0);
            de_esser.set_param("relative_threshold", -12.0);
            de_esser.set_param("range", 24.0);
            de_esser.set_param("band_processing", 1.0);
            de_esser.set_param("sibilance_band", 3.0);
            de_esser.process(&mut output, SR);
            (
                de_esser.gain_reduction_db(),
                -tone_gain_db(&output, &input, 8_000.0, SR as usize, 0),
                -tone_gain_db(&output, &input, 1_000.0, SR as usize, 0),
            )
        };
        for (name, scale) in [("0 dB", 1.0), ("-20 dB", 0.1)] {
            let (gr, sibilance, body) = relative(scale);
            println!(
                "relative {name}: GR {gr:.2} dB, sibilance {sibilance:.2} dB, body {body:.2} dB"
            );
        }

        let frames = SR as usize * 6;
        let mut input = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let t = frame as f64 / SR as f64;
            let db = -34.0 + 20.0 * t / 6.0;
            let amplitude = 10.0_f64.powf(db / 20.0);
            let body = (std::f64::consts::TAU * 1_000.0 * t).sin() * amplitude;
            let sib = (std::f64::consts::TAU * 8_000.0 * t).sin() * amplitude * 0.25;
            input[frame * 2] = (body + sib) as f32;
            input[frame * 2 + 1] = (body + sib) as f32;
        }
        let mut output = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold_mode", 1.0);
        de_esser.set_param("relative_threshold", -12.0);
        de_esser.set_param("range", 24.0);
        de_esser.set_param("band_processing", 1.0);
        de_esser.set_param("sibilance_band", 3.0);
        de_esser.process(&mut output, SR);
        let window = SR as usize;
        let gain = |freq: f64, start: usize| {
            20.0 * (window_amplitude(&output, freq, start, window)
                / window_amplitude(&input, freq, start, window))
            .log10()
        };
        println!(
            "crescendo: body {:.2} -> {:.2} dB, sibilance {:.2} -> {:.2} dB",
            gain(1_000.0, window),
            gain(1_000.0, 5 * window),
            gain(8_000.0, window),
            gain(8_000.0, 5 * window),
        );

        let mut input = sine(1_000.0, 0.2, frames);
        add_tone(&mut input, 8_000.0, 0.05);
        let mut output = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold", -36.0);
        de_esser.set_param("range", 24.0);
        de_esser.process(&mut output, SR);
        println!(
            "full band: GR {:.2} dB, body {:.2} dB",
            de_esser.gain_reduction_db(),
            -tone_gain_db(&output, &input, 1_000.0, 2 * SR as usize, 0),
        );
        let mut output = input.clone();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("threshold", -36.0);
        de_esser.set_param("range", 24.0);
        de_esser.set_param("band_processing", 1.0);
        de_esser.set_param("sibilance_band", 3.0);
        de_esser.process(&mut output, SR);
        println!(
            "split band: GR {:.2} dB, body {:.2} dB",
            de_esser.gain_reduction_db(),
            -tone_gain_db(&output, &input, 1_000.0, 2 * SR as usize, 0),
        );

        let body_only = sine(1_000.0, 0.1, frames);
        for placement in 0..=1 {
            let mut output = body_only.clone();
            let mut de_esser = DeEsser::new(SR);
            de_esser.set_param("threshold", -36.0);
            de_esser.set_param("range", 24.0);
            de_esser.set_param("detector", placement as f64);
            de_esser.process(&mut output, SR);
            println!(
                "placement {placement} on a 1 kHz body: GR {:.2} dB",
                de_esser.gain_reduction_db()
            );
        }
    }

    fn mono_of(stereo: &[f32]) -> Vec<f32> {
        stereo
            .chunks_exact(2)
            .map(|frame| 0.5 * (frame[0] + frame[1]))
            .collect()
    }

    fn add_tone_window(buffer: &mut [f32], freq: f64, amplitude: f64, start: usize, end: usize) {
        for frame in start..end.min(buffer.len() / 2) {
            let value = (std::f64::consts::TAU * freq * frame as f64 / SR as f64).sin() * amplitude;
            buffer[frame * 2] += value as f32;
            buffer[frame * 2 + 1] += value as f32;
        }
    }

    fn synthetic_take(peak_hz: f64, sibilant_hz: bool) -> Vec<f32> {
        let frames = SR as usize * 2;
        let mut buffer = vec![0.0f32; frames * 2];
        add_tone_window(&mut buffer, 1_000.0, 0.1, 0, frames);
        add_tone_window(&mut buffer, 2_000.0, 0.02, 0, frames);
        if sibilant_hz {
            let start = SR as usize * 4 / 5;
            let end = start + SR as usize * 2 / 5;
            for frame in start..end.min(frames) {
                let t = frame as f64 / SR as f64;
                let pulse = 0.75 + 0.25 * (std::f64::consts::TAU * 7.0 * t).sin();
                let value = (std::f64::consts::TAU * peak_hz * t).sin() * 0.1 * pulse;
                buffer[frame * 2] += value as f32;
                buffer[frame * 2 + 1] += value as f32;
            }
        }
        buffer
    }

    fn vowel_sibilant_vowel() -> Vec<f32> {
        let frames = SR as usize;
        let mut buffer = vec![0.0f32; frames * 2];
        add_tone_window(&mut buffer, 1_000.0, 0.1, 0, frames);
        add_tone_window(&mut buffer, 2_000.0, 0.02, 0, frames);
        let start = SR as usize * 3 / 10;
        let end = SR as usize * 6 / 10;
        add_tone_window(&mut buffer, 8_000.0, 0.1, start, end);
        buffer
    }

    fn s7_program(sample_rate: u32) -> Vec<f32> {
        let frames = sample_rate as usize * 6;
        let mut buffer = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let t = frame as f64 / sample_rate as f64;
            let scale = 10.0_f64.powf(-20.0 * t / 6.0 / 20.0);
            let body = (std::f64::consts::TAU * 1_000.0 * t).sin() * 0.1 * scale;
            let sib = (std::f64::consts::TAU * 8_000.0 * t).sin() * 0.1 * scale;
            buffer[frame * 2] = (body + sib) as f32;
            buffer[frame * 2 + 1] = (body + sib) as f32;
        }
        buffer
    }

    fn render_blocks(effect: &mut DeEsser, signal: &[f32], block_frames: usize) -> Vec<f32> {
        let mut output = signal.to_vec();
        let step = block_frames * 2;
        let mut offset = 0;
        while offset < output.len() {
            let end = (offset + step).min(output.len());
            effect.process(&mut output[offset..end], SR);
            offset = end;
        }
        output
    }

    fn active_timeline(effect: &mut DeEsser, signal: &[f32]) -> Vec<(usize, bool)> {
        let mut output = signal.to_vec();
        let step = 128 * 2;
        let mut timeline = Vec::new();
        let mut offset = 0;
        let mut previous = effect.sibilant_active();
        while offset < output.len() {
            let end = (offset + step).min(output.len());
            effect.process(&mut output[offset..end], SR);
            let active = effect.sibilant_active();
            if active != previous {
                timeline.push((offset / 2, active));
                previous = active;
            }
            offset = end;
        }
        timeline
    }

    fn noisy_boundary_sibilant(amplitude: f64, depth: f64) -> Vec<f32> {
        let frames = SR as usize * 2;
        let mut buffer = vec![0.0f32; frames * 2];
        add_tone_window(&mut buffer, 1_000.0, 0.1, 0, frames);
        for frame in 0..frames {
            let t = frame as f64 / SR as f64;
            let modulation = 1.0 + depth * (std::f64::consts::TAU * 3.0 * t).sin();
            let value = (std::f64::consts::TAU * 8_000.0 * t).sin() * amplitude * modulation;
            buffer[frame * 2] += value as f32;
            buffer[frame * 2 + 1] += value as f32;
        }
        buffer
    }

    fn automatic_device() -> DeEsser {
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("band_estimation", 1.0);
        de_esser.set_param("band_processing", 1.0);
        de_esser.set_param("threshold_mode", 1.0);
        de_esser.set_param("relative_threshold", -17.0);
        de_esser.set_param("range", 24.0);
        de_esser
    }

    #[test]
    fn band_estimator_finds_the_sibilant_band_on_three_takes() {
        for peak in [6_000.0, 8_000.0, 11_000.0] {
            let take = mono_of(&synthetic_take(peak, true));
            let band = estimate_sibilance_band(&take, SR).expect("sibilant take");
            assert!(
                (band.peak_hz - peak).abs() / peak < 0.02,
                "{peak} Hz take estimated peak {}",
                band.peak_hz
            );
            assert!(
                (band.band_hz - peak / 1.4).abs() / (peak / 1.4) < 0.03,
                "{peak} Hz take band {}",
                band.band_hz
            );
            assert!(
                (band.top_hz - band.band_hz * 1.8667).abs() / band.top_hz < 0.03,
                "{peak} Hz take top {}",
                band.top_hz
            );
            assert!(band.band_hz < peak && peak < band.top_hz);
            assert!(band.frames >= 8, "{peak} Hz frames {}", band.frames);
        }
    }

    #[test]
    fn band_estimator_reports_none_without_sibilance() {
        let vowel = mono_of(&synthetic_take(8_000.0, false));
        assert_eq!(estimate_sibilance_band(&vowel, SR), None);
        assert_eq!(estimate_sibilance_band(&[], SR), None);
        assert_eq!(estimate_sibilance_band(&vec![0.0; SR as usize], SR), None);
    }

    #[test]
    fn running_estimator_locks_the_band_and_reports_it() {
        let take = synthetic_take(8_000.0, true);
        let mut de_esser = automatic_device();
        let output = render_blocks(&mut de_esser, &take, 480);
        assert_eq!(output.len(), take.len());
        let (band, top) = de_esser.band_edges_hz();
        assert!((band - 5714.0).abs() < 300.0, "band {band}");
        assert!((top - 10_666.0).abs() < 500.0, "top {top}");
        assert_eq!(de_esser.detected_band_hz(), Some(band));
        assert_eq!(
            de_esser.get_param("detected_band"),
            Some(de_esser.detected_band_hz().unwrap())
        );
        assert_eq!(
            de_esser.get_param("band_override"),
            Some(0.0),
            "the override stays the configured value"
        );
        assert_eq!(de_esser.get_param("chosen_band"), Some(band));
    }

    #[test]
    fn automatic_band_does_not_act_without_sibilance() {
        let take = synthetic_take(8_000.0, false);
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("band_estimation", 1.0);
        de_esser.set_param("detector", 1.0);
        de_esser.set_param("range", 24.0);
        let output = render_blocks(&mut de_esser, &take, 480);
        assert_eq!(output, take, "no sibilance means no reduction");
        assert_eq!(de_esser.detected_band_hz(), None);
        assert_eq!(
            de_esser.band_edges_hz(),
            (DEFAULT_HIGH_HZ, DEFAULT_LOW_HZ),
            "fallback is the shipped default band"
        );
        assert_eq!(de_esser.gain_reduction_db(), 0.0);
    }

    #[test]
    fn segment_detector_fires_inside_the_sibilant_not_in_the_vowel() {
        let signal = vowel_sibilant_vowel();
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("sibilance_band", 2.0);
        de_esser.set_param("segment_detection", 1.0);
        let timeline = active_timeline(&mut de_esser, &signal);
        let seconds = |frame: usize| frame as f64 / SR as f64;
        assert_eq!(timeline.len(), 2, "one segment: {timeline:?}");
        let (enter, on) = timeline[0];
        let (exit, off) = timeline[1];
        assert!(on && !off);
        assert!(
            (0.28..=0.34).contains(&seconds(enter)),
            "entered at {:.3}s",
            seconds(enter)
        );
        assert!(
            (0.58..=0.66).contains(&seconds(exit)),
            "left at {:.3}s",
            seconds(exit)
        );
    }

    #[test]
    fn segment_detector_hysteresis_does_not_chatter_on_a_noisy_boundary() {
        // +/-2 dB around the -12 dB share boundary: enough to cross it, but a
        // single-threshold detector would toggle every modulation cycle.
        let signal = noisy_boundary_sibilant(0.025, 0.2);
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("sibilance_band", 3.0);
        de_esser.set_param("segment_detection", 1.0);
        let timeline = active_timeline(&mut de_esser, &signal);
        println!("noisy timeline {timeline:?}");
        assert_eq!(timeline.len(), 1, "one entry, no chatter: {timeline:?}");
        assert!(timeline[0].1);

        // The same modulation floor never reaches the enter share on its own.
        let quiet = noisy_boundary_sibilant(0.0175, 0.2);
        let mut quiet_device = DeEsser::new(SR);
        quiet_device.set_param("sibilance_band", 3.0);
        quiet_device.set_param("segment_detection", 1.0);
        assert!(active_timeline(&mut quiet_device, &quiet).is_empty());
    }

    #[test]
    fn automatic_split_band_meets_the_six_db_and_half_db_gates() {
        let signal = s7_program(SR);
        let mut de_esser = automatic_device();
        de_esser.set_param("segment_detection", 1.0);
        let output = render_blocks(&mut de_esser, &signal, 480);
        let window = SR as usize / 2;
        for start in [SR as usize, 3 * SR as usize, 5 * SR as usize] {
            let body = 20.0
                * (window_amplitude(&output, 1_000.0, start, window)
                    / window_amplitude(&signal, 1_000.0, start, window))
                .log10();
            let sibilance = 20.0
                * (window_amplitude(&output, 8_000.0, start, window)
                    / window_amplitude(&signal, 8_000.0, start, window))
                .log10();
            assert!(
                (-7.0..=-5.0).contains(&sibilance),
                "sibilance {sibilance:.2} dB at {start}"
            );
            assert!(body.abs() <= 0.5, "body {body:.2} dB at {start}");
        }
    }

    #[test]
    fn band_override_pins_the_band() {
        let mut de_esser = DeEsser::new(SR);
        de_esser.set_param("band_estimation", 1.0);
        de_esser.set_param("band_override", 8_000.0);
        let take = synthetic_take(11_000.0, true);
        let _ = render_blocks(&mut de_esser, &take, 480);
        assert_eq!(de_esser.detected_band_hz(), Some(8_000.0));
        let (band, top) = de_esser.band_edges_hz();
        assert_eq!(band, 8_000.0);
        assert!((top - 8_000.0 * 1.8667).abs() < 1.0);
        assert_eq!(de_esser.get_param("band_override"), Some(8_000.0));
        assert_eq!(de_esser.get_param("chosen_band"), Some(8_000.0));
    }

    #[test]
    fn chosen_band_reports_the_mode_in_use() {
        let mut de_esser = DeEsser::new(SR);
        assert_eq!(de_esser.get_param("chosen_band"), Some(DEFAULT_HIGH_HZ));
        de_esser.set_param("sibilance_band", 3.0);
        assert_eq!(de_esser.get_param("chosen_band"), Some(7_000.0));
        assert_eq!(de_esser.get_param("detected_band"), Some(0.0));
        de_esser.set_param("band_override", 5_400.0);
        assert_eq!(de_esser.get_param("chosen_band"), Some(5_400.0));
        assert_eq!(de_esser.get_param("detected_band"), Some(5_400.0));
    }

    #[test]
    fn automatic_band_is_block_size_invariant() {
        let signal = s7_program(SR);
        let mut reference = automatic_device();
        reference.set_param("segment_detection", 1.0);
        let expected = render_blocks(&mut reference, &signal, 480);
        for block in [97, 240, 1_024] {
            let mut de_esser = automatic_device();
            de_esser.set_param("segment_detection", 1.0);
            let output = render_blocks(&mut de_esser, &signal, block);
            assert_eq!(output, expected, "block {block}");
        }
        let mut again = automatic_device();
        again.set_param("segment_detection", 1.0);
        assert_eq!(render_blocks(&mut again, &signal, 480), expected);
        assert_eq!(
            again.band_edges_hz(),
            reference.band_edges_hz(),
            "deterministic band"
        );
    }

    #[test]
    fn reset_repeats_the_automatic_path() {
        let signal = s7_program(SR);
        let mut de_esser = automatic_device();
        de_esser.set_param("segment_detection", 1.0);
        let first = render_blocks(&mut de_esser, &signal, 480);
        de_esser.reset();
        assert_eq!(de_esser.detected_band_hz(), None, "reset forgets the band");
        let second = render_blocks(&mut de_esser, &signal, 480);
        assert_eq!(first, second);
    }

    #[test]
    fn zero_range_is_bit_identical_with_the_automatic_path_enabled() {
        let signal = s7_program(SR);
        let mut de_esser = automatic_device();
        de_esser.set_param("segment_detection", 1.0);
        de_esser.set_param("range", 0.0);
        let latency = de_esser.latency_samples() as usize;
        assert_eq!(
            latency, SPLIT_DELAY,
            "automatic split mode reports its delay"
        );
        let mut drained = signal.clone();
        drained.resize(signal.len() + latency * 2, 0.0);
        let output = render_blocks(&mut de_esser, &drained, 480);
        for (sample, value) in output[..latency * 2].iter().enumerate() {
            assert_eq!(value.to_bits(), 0.0f32.to_bits(), "startup sample {sample}");
        }
        for (sample, (actual, expected)) in output[latency * 2..].iter().zip(&signal).enumerate() {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "zero-range PCM differs at input sample {sample}, reported latency {latency} frames"
            );
        }
        assert_eq!(de_esser.gain_reduction_db(), 0.0);
    }

    #[test]
    #[ignore]
    fn measurements_for_automatic_band() {
        for peak in [6_000.0, 8_000.0, 11_000.0] {
            let take = mono_of(&synthetic_take(peak, true));
            let band = estimate_sibilance_band(&take, SR).expect("estimated");
            println!(
                "take {peak:.0} Hz: peak {:.1}, band {:.1}, top {:.1}, frames {}",
                band.peak_hz, band.band_hz, band.top_hz, band.frames
            );
        }
        let vowel = mono_of(&synthetic_take(8_000.0, false));
        println!("vowel only: {:?}", estimate_sibilance_band(&vowel, SR));

        let signal = vowel_sibilant_vowel();
        let mut segment = DeEsser::new(SR);
        segment.set_param("sibilance_band", 2.0);
        segment.set_param("segment_detection", 1.0);
        println!("segment edges {:?}", active_timeline(&mut segment, &signal));

        let mut shares = DeEsser::new(SR);
        shares.set_param("sibilance_band", 2.0);
        shares.set_param("segment_detection", 1.0);
        let mut output = signal.clone();
        let mut vowel_share: Option<(f64, f64)> = None;
        let mut sibilant_share: Option<(f64, f64)> = None;
        let mut offset = 0;
        while offset < output.len() {
            let end = (offset + 512).min(output.len());
            shares.process(&mut output[offset..end], SR);
            let at = (offset / 2) as f64 / SR as f64;
            let share = shares.analyzer.share_db;
            if (0.1..0.25).contains(&at) {
                let slot = vowel_share.get_or_insert((share, share));
                slot.0 = slot.0.min(share);
                slot.1 = slot.1.max(share);
            }
            if (0.35..0.55).contains(&at) {
                let slot = sibilant_share.get_or_insert((share, share));
                slot.0 = slot.0.min(share);
                slot.1 = slot.1.max(share);
            }
            offset = end;
        }
        println!("vowel share {vowel_share:?} sibilant share {sibilant_share:?}");

        let s7 = s7_program(SR);
        let mut de_esser = automatic_device();
        de_esser.set_param("segment_detection", 1.0);
        let output = render_blocks(&mut de_esser, &s7, 480);
        println!(
            "s7 edges {:?} peak {:?}",
            de_esser.band_edges_hz(),
            de_esser.estimated_peak_hz()
        );
        let window = SR as usize / 2;
        for start in [SR as usize, 3 * SR as usize, 5 * SR as usize] {
            let body = 20.0
                * (window_amplitude(&output, 1_000.0, start, window)
                    / window_amplitude(&s7, 1_000.0, start, window))
                .log10();
            let sibilance = 20.0
                * (window_amplitude(&output, 8_000.0, start, window)
                    / window_amplitude(&s7, 8_000.0, start, window))
                .log10();
            println!(
                "s7 {:.1}s: body {:.2} dB, sibilance {:.2} dB, GR {:.2} dB",
                start as f64 / SR as f64,
                body,
                sibilance,
                de_esser.gain_reduction_db()
            );
        }
    }
}
