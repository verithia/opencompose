//! Feed-forward, stereo-linked compressor with a peak detector and two-stage release.
//!
//! Engineering controls and normalized controls resolve to the same detector,
//! transfer curve and timing coefficients. Lookahead delays the signal while
//! the sidechain filters affect detection only. The device schema owns the
//! parameter ranges, defaults and units. Compatibility controls `detector` and
//! `mode` are accepted without selecting a different processing algorithm.

use super::AudioEffect;
use super::biquad::{BiquadFilter, FilterType};

const MAX_LOOKAHEAD_MS: f64 = 20.0;
const HPF_BYPASS_HZ: f64 = 20.0;
const DB_FLOOR: f64 = -180.0;
/// Detector-only filter bands; one biquad each, no slope cascade.
pub const SIDECHAIN_EQ_BANDS: usize = 6;
/// Low and high limits shared with the device schema.
const SIDECHAIN_EQ_MIN_HZ: f64 = 20.0;
const SIDECHAIN_EQ_MAX_HZ: f64 = 20_000.0;
const SIDECHAIN_EQ_MAX_GAIN_DB: f64 = 30.0;
const SIDECHAIN_EQ_MIN_Q: f64 = 0.1;
const SIDECHAIN_EQ_MAX_Q: f64 = 10.0;
/// Share of the release state assigned to the fast capacitor.
const FAST_SHARE: f64 = 0.34;
/// The detector holds a sample peak for this long, bridging the ripple of a
/// steady tone so the static curve is reached at the crest.
const PEAK_HOLD_S: f64 = 0.0012;
/// Auto Release slows the fast release stage by this factor after a transient.
const AUTO_RELEASE_FAST_SCALE: f64 = 1.3;
const INV_E: f64 = 0.367_879_441_171_442_33;
/// Resolution of the hold-dependent release coefficient tables.
const HOLD_TABLE_LEN: usize = 65;

/// Normalized ratio control mapped to compression slope `S = 1 - 1/R`.
const RATIO_CURVE: [(f64, f64); 22] = [
    (0.00, 0.0000),
    (0.10, 0.0909),
    (0.15, 0.1489),
    (0.20, 0.2000),
    (0.25, 0.2727),
    (0.30, 0.3333),
    (0.35, 0.4286),
    (0.40, 0.5000),
    (0.45, 0.5789),
    (0.50, 0.6364),
    (0.55, 0.7037),
    (0.60, 0.7500),
    (0.65, 0.8000),
    (0.70, 0.8333),
    (0.75, 0.8571),
    (0.80, 0.8750),
    (0.85, 0.8889),
    (0.90, 0.9000),
    (0.95, 0.9590),
    (0.98, 0.9851),
    (0.99, 0.9891),
    (1.00, 1.0000),
];

/// Effective attack time constant at 10 dB above the threshold.
const ATTACK_TAU_MS: [(f64, f64); 5] = [
    (0.0, 0.51),
    (0.1, 0.79),
    (0.3, 7.39),
    (0.6, 70.8),
    (1.0, 1100.0),
];

/// Two-stage release constants: `(plain, tau_fast_ms, tau_slow_ms)`.
const RELEASE_SHORT: [(f64, f64, f64); 3] =
    [(0.20, 6.8, 23.2), (0.40, 23.6, 81.1), (0.70, 75.7, 265.3)];
const RELEASE_LONG: [(f64, f64, f64); 3] =
    [(0.20, 2.2, 110.0), (0.40, 8.6, 147.3), (0.70, 36.9, 278.9)];

/// A compressor block refused before audio or detector history is changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressorProcessError {
    IncompleteFrame,
    IncompleteSidechainFrame,
    SampleRateChanged,
    NonFiniteAudio,
    NonFiniteSidechain,
}
impl std::fmt::Display for CompressorProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::IncompleteFrame => "Compressor input must contain complete stereo frames",
            Self::IncompleteSidechainFrame => {
                "Compressor sidechain must contain complete stereo frames"
            }
            Self::SampleRateChanged => {
                "Compressor sample rate changed; construct the device at the project rate"
            }
            Self::NonFiniteAudio => "Compressor audio contains a non-finite sample",
            Self::NonFiniteSidechain => "Compressor sidechain contains a non-finite sample",
        })
    }
}
impl std::error::Error for CompressorProcessError {}

#[derive(Clone, Copy)]
enum DetectorInput<'a> {
    Audio,
    Stereo(&'a [f32]),
    Control(&'a [f32]),
}

/// A soft-knee stereo compressor with lookahead and an optional sidechain.
pub struct Compressor {
    threshold_db: f64,
    ratio: f64,
    ratio_plain: f64,
    compression_slope: f64,
    attack_ms: f64,
    attack_plain: f64,
    release_ms: f64,
    release_plain: f64,
    knee_db: f64,
    range_db: f64,
    makeup_db: f64,
    lookahead_ms: f64,
    max_lookahead_ms: f64,
    detector_hpf_hz: f64,
    detector_rms: bool,
    mix: f64,
    auto_makeup: bool,
    stereo_link: f64,
    hold_ms: f64,
    auto_release: bool,
    mode: f64,
    // Internal state
    attack_coeff: f64,
    fast_coeffs: [f64; HOLD_TABLE_LEN],
    slow_coeffs: [f64; HOLD_TABLE_LEN],
    fast: [f64; 2],
    slow: [f64; 2],
    sustain_ms: [f64; 2],
    hold_remaining: [f64; 2],
    held_level: [f64; 2],
    peak_hold: [f64; 2],
    hpf: [BiquadFilter; 2],
    hpf_active: bool,
    sidechain_eq: [BiquadFilter; SIDECHAIN_EQ_BANDS],
    sidechain_eq_type: [FilterType; SIDECHAIN_EQ_BANDS],
    sidechain_eq_frequency: [f64; SIDECHAIN_EQ_BANDS],
    sidechain_eq_gain: [f64; SIDECHAIN_EQ_BANDS],
    sidechain_eq_q: [f64; SIDECHAIN_EQ_BANDS],
    sidechain_eq_on: [bool; SIDECHAIN_EQ_BANDS],
    sidechain_eq_active: bool,
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    key_l: Vec<f32>,
    key_r: Vec<f32>,
    delay_pos: usize,
    last_gr_db: f64,
    sample_rate: u32,
    process_error: Option<CompressorProcessError>,
    error_reported: std::cell::Cell<bool>,
}

impl Compressor {
    /// Prepare a compressor with lookahead buffers for the sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let max_frames = ((sample_rate as f64 * MAX_LOOKAHEAD_MS * 0.001) as usize).max(1);
        let hpf = || BiquadFilter::new(FilterType::HighPass, 20.0, 0.707, 0.0, sample_rate);
        let mut c = Self {
            threshold_db: -12.0,
            ratio: 4.0,
            ratio_plain: ratio_to_plain(4.0),
            compression_slope: ratio_plain_to_s(ratio_to_plain(4.0)),
            attack_ms: 10.0,
            attack_plain: attack_plain_from_ms(10.0),
            release_ms: 100.0,
            release_plain: release_plain_from_ms(100.0),
            knee_db: 6.0,
            range_db: 60.0,
            makeup_db: 0.0,
            lookahead_ms: 0.0,
            max_lookahead_ms: MAX_LOOKAHEAD_MS,
            detector_hpf_hz: HPF_BYPASS_HZ,
            detector_rms: false,
            mix: 1.0,
            auto_makeup: false,
            stereo_link: 1.0,
            hold_ms: 0.0,
            auto_release: false,
            mode: 0.0,
            attack_coeff: 0.0,
            fast_coeffs: [0.0; HOLD_TABLE_LEN],
            slow_coeffs: [0.0; HOLD_TABLE_LEN],
            fast: [0.0; 2],
            slow: [0.0; 2],
            sustain_ms: [0.0; 2],
            hold_remaining: [0.0; 2],
            held_level: [0.0; 2],
            peak_hold: [0.0; 2],
            hpf: [hpf(), hpf()],
            hpf_active: false,
            sidechain_eq: std::array::from_fn(|_| {
                BiquadFilter::new(FilterType::Peak, 1_000.0, 0.707, 0.0, sample_rate)
            }),
            sidechain_eq_type: [FilterType::Peak; SIDECHAIN_EQ_BANDS],
            sidechain_eq_frequency: [1_000.0; SIDECHAIN_EQ_BANDS],
            sidechain_eq_gain: [0.0; SIDECHAIN_EQ_BANDS],
            sidechain_eq_q: [0.707; SIDECHAIN_EQ_BANDS],
            sidechain_eq_on: [false; SIDECHAIN_EQ_BANDS],
            sidechain_eq_active: false,
            delay_l: vec![0.0; max_frames + 1],
            delay_r: vec![0.0; max_frames + 1],
            key_l: vec![0.0; max_frames + 1],
            key_r: vec![0.0; max_frames + 1],
            delay_pos: 0,
            last_gr_db: 0.0,
            sample_rate,
            process_error: None,
            error_reported: std::cell::Cell::new(false),
        };
        c.rebuild_coefficients();
        c
    }

    /// Prepare attack and hold-indexed release coefficients. Parameter edits
    /// rebuild only the component they change; processing interpolates the
    /// release tables and retains the exact dynamic gain conversion.
    fn rebuild_coefficients(&mut self) {
        self.rebuild_attack();
        self.rebuild_release();
    }

    fn rebuild_attack(&mut self) {
        let tau_att = attack_tau_ms(self.attack_plain).max(0.05);
        self.attack_coeff = (-1.0 / (tau_att * 0.001 * self.sample_rate as f64)).exp();
    }

    fn rebuild_release(&mut self) {
        let sr = self.sample_rate as f64;
        let (short_fast, short_slow) = release_pair(self.release_plain, false);
        let (long_fast, long_slow) = release_pair(self.release_plain, true);
        let fast_log = short_fast.ln();
        let fast_span = long_fast.ln() - fast_log;
        let slow_log = short_slow.ln();
        let slow_span = long_slow.ln() - slow_log;
        for index in 0..HOLD_TABLE_LEN {
            let hold = index as f64 / (HOLD_TABLE_LEN - 1) as f64;
            let mut tau_fast = (fast_log + fast_span * hold).exp();
            let tau_slow = (slow_log + slow_span * hold).exp();
            if self.auto_release {
                tau_fast *= 1.0 + (AUTO_RELEASE_FAST_SCALE - 1.0) * (1.0 - hold);
            }
            self.fast_coeffs[index] = (-1.0 / (tau_fast.max(0.05) * 0.001 * sr)).exp();
            self.slow_coeffs[index] = (-1.0 / (tau_slow.max(0.05) * 0.001 * sr)).exp();
        }
    }

    fn release_coefficients(&self, hold: f64) -> (f64, f64) {
        let x = (hold.clamp(0.0, 1.0) * (HOLD_TABLE_LEN - 1) as f64)
            .min((HOLD_TABLE_LEN - 1) as f64 - 1.0e-9);
        let index = x as usize;
        let fraction = x - index as f64;
        let fast = self.fast_coeffs[index]
            + (self.fast_coeffs[index + 1] - self.fast_coeffs[index]) * fraction;
        let slow = self.slow_coeffs[index]
            + (self.slow_coeffs[index + 1] - self.slow_coeffs[index]) * fraction;
        (fast, slow)
    }

    fn reset_delay(&mut self) {
        self.delay_l.fill(0.0);
        self.delay_r.fill(0.0);
        self.key_l.fill(0.0);
        self.key_r.fill(0.0);
        self.delay_pos = 0;
    }

    fn update_detector_hpf(&mut self) {
        self.hpf_active = self.detector_hpf_hz > HPF_BYPASS_HZ;
        if self.hpf_active {
            for filter in &mut self.hpf {
                filter.set_params(self.detector_hpf_hz, 0.707, 0.0);
            }
        } else {
            for filter in &mut self.hpf {
                filter.reset();
            }
        }
    }

    /// Push one band's stored parameters into its biquad and refresh the
    /// "any band enabled" fast path.
    fn update_sidechain_eq(&mut self, band: usize) {
        self.sidechain_eq[band].set_type(self.sidechain_eq_type[band]);
        self.sidechain_eq[band].set_params(
            self.sidechain_eq_frequency[band],
            self.sidechain_eq_q[band],
            self.sidechain_eq_gain[band],
        );
        self.sidechain_eq_active = self.sidechain_eq_on.iter().any(|on| *on);
    }

    fn auto_makeup_db(&self) -> f64 {
        let slope = self.compression_slope;
        slope * (-self.threshold_db) * INV_E
    }

    /// Static gain reduction for a detector level in dB (positive = reduction).
    fn static_gr_db(&self, input_db: f64) -> f64 {
        let slope = self.compression_slope;
        let over = input_db - self.threshold_db;
        let gain = if self.knee_db <= 1.0e-7 {
            over.max(0.0)
        } else {
            let half = self.knee_db / 2.0;
            if over <= -half {
                0.0
            } else if over >= half {
                over
            } else {
                half * ((over + half) / self.knee_db).powi(2)
            }
        };
        (slope * gain).clamp(0.0, self.range_db)
    }

    fn latency_frames(&self) -> usize {
        if self.lookahead_ms <= 0.0 || self.max_lookahead_ms <= 0.0 {
            return 0;
        }
        let max_frames = self.delay_l.len() - 1;
        ((self.sample_rate as f64 * self.max_lookahead_ms * 0.001) as usize).min(max_frames)
    }
}

impl Compressor {
    fn process_detector(&mut self, buffer: &mut [f32], input: DetectorInput<'_>) {
        let sr = self.sample_rate as f64;
        let dt = 1.0 / sr;
        let makeup_lin = db_to_linear(
            self.makeup_db
                + if self.auto_makeup {
                    self.auto_makeup_db()
                } else {
                    0.0
                },
        );
        let ring_len = self.delay_l.len();
        let engaged = self.lookahead_ms > 0.0 && self.max_lookahead_ms > 0.0;
        let max_frames = if engaged {
            ((sr * self.max_lookahead_ms * 0.001) as usize).min(ring_len - 1)
        } else {
            0
        };
        let knob_frames = if engaged {
            ((sr * self.lookahead_ms * 0.001) as usize).min(max_frames)
        } else {
            0
        };
        // Audio is delayed by the whole line; the knob moves the detector tap
        // from the output-aligned sample towards the live input.
        let audio_delay = max_frames;
        let detector_delay = max_frames - knob_frames;
        let link = self.stereo_link;
        let hold_seconds = self.hold_ms * 0.001;
        let len = buffer.len();
        let mut i = 0;

        while i + 1 < len {
            let in_l = buffer[i] as f64;
            let in_r = buffer[i + 1] as f64;
            let (mut key_in_l, mut key_in_r) = match input {
                DetectorInput::Audio => (in_l, in_r),
                DetectorInput::Stereo(key) => (
                    key.get(i).copied().unwrap_or(0.0) as f64,
                    key.get(i + 1).copied().unwrap_or(0.0) as f64,
                ),
                DetectorInput::Control(control) => {
                    let value = control.get(i / 2).copied().unwrap_or(0.0);
                    let level = if value.is_finite() {
                        value.clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    (level as f64, level as f64)
                }
            };
            if self.hpf_active {
                key_in_l = self.hpf[0].process_sample(0, key_in_l);
                key_in_r = self.hpf[1].process_sample(1, key_in_r);
            }
            if self.sidechain_eq_active {
                let enabled = self.sidechain_eq_on;
                for (index, &is_enabled) in enabled.iter().enumerate() {
                    if !is_enabled {
                        continue;
                    }
                    key_in_l = self.sidechain_eq[index].process_sample(0, key_in_l);
                    key_in_r = self.sidechain_eq[index].process_sample(1, key_in_r);
                }
            }
            self.key_l[self.delay_pos] = key_in_l as f32;
            self.key_r[self.delay_pos] = key_in_r as f32;
            self.delay_l[self.delay_pos] = buffer[i];
            self.delay_r[self.delay_pos] = buffer[i + 1];

            let detector_pos = (self.delay_pos + ring_len - detector_delay) % ring_len;
            let raw_l = (self.key_l[detector_pos] as f64).abs();
            let raw_r = (self.key_r[detector_pos] as f64).abs();
            for (channel, raw) in [raw_l, raw_r].into_iter().enumerate() {
                if raw >= self.held_level[channel] {
                    self.held_level[channel] = raw;
                    self.peak_hold[channel] = PEAK_HOLD_S;
                } else if self.peak_hold[channel] > 0.0 {
                    self.peak_hold[channel] -= dt;
                } else {
                    self.held_level[channel] = raw;
                }
            }
            let level_l = self.held_level[0];
            let level_r = self.held_level[1];
            let peak = level_l.max(level_r);
            let key_l = level_l + link * (peak - level_l);
            let key_r = level_r + link * (peak - level_r);

            let audio_pos = (self.delay_pos + ring_len - audio_delay) % ring_len;
            let dry_l = self.delay_l[audio_pos] as f64;
            let dry_r = self.delay_r[audio_pos] as f64;

            let mut gr = [0.0_f64; 2];
            for (channel, key) in [key_l, key_r].into_iter().enumerate() {
                let target = self.static_gr_db(linear_to_db(key));
                let current = self.fast[channel] + self.slow[channel];
                if target > 0.005 {
                    self.sustain_ms[channel] = (self.sustain_ms[channel] + dt * 1000.0).min(2000.0);
                } else if current < 0.005 {
                    self.sustain_ms[channel] = 0.0;
                }
                if target > current {
                    let next = current + (target - current) * (1.0 - self.attack_coeff);
                    self.fast[channel] = next * FAST_SHARE;
                    self.slow[channel] = next * (1.0 - FAST_SHARE);
                    self.hold_remaining[channel] = hold_seconds;
                } else if self.hold_remaining[channel] > 0.0 {
                    self.hold_remaining[channel] -= dt;
                } else {
                    let hold = self.sustain_ms[channel] * 0.001;
                    let (fast_coeff, slow_coeff) = self.release_coefficients(hold);
                    let fast_target = target * FAST_SHARE;
                    let slow_target = target * (1.0 - FAST_SHARE);
                    self.fast[channel] =
                        fast_target + (self.fast[channel] - fast_target) * fast_coeff;
                    self.slow[channel] =
                        slow_target + (self.slow[channel] - slow_target) * slow_coeff;
                }
                gr[channel] = self.fast[channel] + self.slow[channel];
            }
            self.last_gr_db = gr[0].max(gr[1]);

            let gain_l = db_to_linear(-gr[0]) * makeup_lin;
            let gain_r = db_to_linear(-gr[1]) * makeup_lin;
            let wet_l = dry_l * (1.0 + self.mix * (gain_l - 1.0));
            let wet_r = dry_r * (1.0 + self.mix * (gain_r - 1.0));
            buffer[i] = wet_l as f32;
            buffer[i + 1] = wet_r as f32;

            self.delay_pos = if self.delay_pos + 1 == ring_len {
                0
            } else {
                self.delay_pos + 1
            };
            i += 2;
        }
    }
}

impl Compressor {
    /// Acknowledge a refused host request without changing signal history.
    /// All compressor failures are input preflight refusals, never partial DSP.
    pub fn acknowledge_processing_error(&mut self) -> Option<CompressorProcessError> {
        self.error_reported.set(false);
        self.process_error.take()
    }

    fn process_input(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        input: DetectorInput<'_>,
    ) -> Result<(), CompressorProcessError> {
        let error = self.process_error.or_else(|| {
            if sample_rate != self.sample_rate {
                return Some(CompressorProcessError::SampleRateChanged);
            }
            if !buffer.len().is_multiple_of(2) {
                return Some(CompressorProcessError::IncompleteFrame);
            }
            if buffer.iter().any(|sample| !sample.is_finite()) {
                return Some(CompressorProcessError::NonFiniteAudio);
            }
            if let DetectorInput::Stereo(key) = input {
                if !key.len().is_multiple_of(2) {
                    return Some(CompressorProcessError::IncompleteSidechainFrame);
                }
                if key.iter().any(|sample| !sample.is_finite()) {
                    return Some(CompressorProcessError::NonFiniteSidechain);
                }
            }
            None
        });
        if let Some(error) = error {
            if self.process_error.is_none() {
                self.process_error = Some(error);
                self.error_reported.set(false);
            }
            return Err(error);
        }
        self.process_detector(buffer, input);
        Ok(())
    }

    /// Compress a stereo block; returns an error when the input is malformed.
    pub fn try_process(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
    ) -> Result<(), CompressorProcessError> {
        self.process_input(buffer, sample_rate, DetectorInput::Audio)
    }
    /// Short complete stereo keys end in silence; they never fall back to the
    /// main input. The key and audio buffers are validated before processing.
    pub fn try_process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        key: &[f32],
        sample_rate: u32,
    ) -> Result<(), CompressorProcessError> {
        self.process_input(buffer, sample_rate, DetectorInput::Stereo(key))
    }
    /// Read mono control directly, without materializing an interleaved key.
    /// Non-finite control values retain their established silence semantics.
    pub fn try_process_with_control(
        &mut self,
        buffer: &mut [f32],
        control: &[f32],
        sample_rate: u32,
    ) -> Result<(), CompressorProcessError> {
        self.process_input(buffer, sample_rate, DetectorInput::Control(control))
    }
}

impl AudioEffect for Compressor {
    fn prepare_block(&mut self, max_frames: usize) -> Result<(), oc_audio::BlockPreparationError> {
        max_frames
            .checked_mul(2)
            .filter(|samples| *samples <= isize::MAX as usize / std::mem::size_of::<f32>())
            .ok_or(oc_audio::BlockPreparationError::CapacityExceeded)?;
        // Lookahead and filters are fully prepared at construction. All input
        // modes process arbitrarily bounded slices directly without scratch.
        Ok(())
    }
    fn processing_failed(&self) -> bool {
        self.process_error.is_some()
    }
    fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        match self.process_error {
            None => oc_audio::ProcessingFailureReport::Unavailable,
            Some(_) if self.error_reported.replace(true) => {
                oc_audio::ProcessingFailureReport::Reported
            }
            Some(error) => oc_audio::ProcessingFailureReport::Ready(error.to_string()),
        }
    }
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        let _ = self.try_process(buffer, sample_rate);
    }
    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        sample_rate: u32,
        _transport: Option<oc_audio::PluginTransport>,
    ) {
        let _ = self.try_process_with_sidechain(buffer, sidechain, sample_rate);
    }

    fn supports_sidechain(&self) -> bool {
        true
    }

    /// A control input drives the same detector as a key, but the value is the
    /// already-normalized control signal: `0.0` is silence and `1.0` is 0 dBFS.
    /// The key is mono, so both detector channels read the same level.
    fn process_with_control(
        &mut self,
        buffer: &mut [f32],
        control: &[f32],
        sample_rate: u32,
        _transport: Option<oc_audio::PluginTransport>,
    ) {
        let _ = self.try_process_with_control(buffer, control, sample_rate);
    }

    fn supports_control_input(&self) -> bool {
        true
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "threshold" => self.threshold_db = value.clamp(-60.0, 0.0),
            "ratio" => {
                self.ratio = value.clamp(1.0, 100.0);
                self.ratio_plain = ratio_to_plain(self.ratio);
                self.compression_slope = ratio_plain_to_s(self.ratio_plain);
            }
            "ratio_plain" => {
                self.ratio_plain = value.clamp(0.0, 1.0);
                self.ratio = plain_to_ratio(self.ratio_plain);
                self.compression_slope = ratio_plain_to_s(self.ratio_plain);
            }
            "attack" => {
                self.attack_ms = value.clamp(0.1, 200.0);
                let next = attack_plain_from_ms(self.attack_ms);
                if next != self.attack_plain {
                    self.attack_plain = next;
                    self.rebuild_attack();
                }
            }
            "attack_plain" => {
                let next = value.clamp(0.0, 1.0);
                self.attack_ms = attack_tau_ms(next);
                if next != self.attack_plain {
                    self.attack_plain = next;
                    self.rebuild_attack();
                }
            }
            "release" => {
                self.release_ms = value.clamp(10.0, 2000.0);
                let next = release_plain_from_ms(self.release_ms);
                if next != self.release_plain {
                    self.release_plain = next;
                    self.rebuild_release();
                }
            }
            "release_plain" => {
                let next = value.clamp(0.0, 1.0);
                self.release_ms = release_pair(next, false).1;
                if next != self.release_plain {
                    self.release_plain = next;
                    self.rebuild_release();
                }
            }
            "knee" => self.knee_db = value.clamp(0.0, 72.0),
            "range" => self.range_db = value.clamp(0.0, 60.0),
            "makeup" => self.makeup_db = value.clamp(-12.0, 24.0),
            "lookahead" => self.lookahead_ms = value.clamp(0.0, MAX_LOOKAHEAD_MS),
            "max_lookahead" => {
                let next = value.clamp(0.0, MAX_LOOKAHEAD_MS);
                if (next - self.max_lookahead_ms).abs() > f64::EPSILON {
                    self.max_lookahead_ms = next;
                    self.reset_delay();
                }
            }
            "detector_hpf" => {
                self.detector_hpf_hz = value.clamp(HPF_BYPASS_HZ, 500.0);
                self.update_detector_hpf();
            }
            "stereo_link" => self.stereo_link = value.clamp(0.0, 1.0),
            "hold" => self.hold_ms = value.clamp(0.0, 500.0),
            "auto_release" => {
                let next = value >= 0.5;
                if next != self.auto_release {
                    self.auto_release = next;
                    self.rebuild_release();
                }
            }
            "detector" => self.detector_rms = value >= 0.5,
            "mix" => self.mix = value.clamp(0.0, 1.0),
            "auto_makeup" => self.auto_makeup = value >= 0.5,
            "mode" => self.mode = value,
            _ => {
                let Some((band, field)) = sidechain_eq_param(name) else {
                    return;
                };
                match field {
                    "on" => {
                        let on = value >= 0.5;
                        if on != self.sidechain_eq_on[band] {
                            self.sidechain_eq_on[band] = on;
                            self.sidechain_eq[band].reset();
                            self.update_sidechain_eq(band);
                        }
                    }
                    "type" => {
                        if let Some(filter) = sidechain_eq_filter(value) {
                            self.sidechain_eq_type[band] = filter;
                            self.update_sidechain_eq(band);
                        }
                    }
                    "frequency" => {
                        self.sidechain_eq_frequency[band] =
                            value.clamp(SIDECHAIN_EQ_MIN_HZ, SIDECHAIN_EQ_MAX_HZ);
                        self.update_sidechain_eq(band);
                    }
                    "gain" => {
                        self.sidechain_eq_gain[band] =
                            value.clamp(-SIDECHAIN_EQ_MAX_GAIN_DB, SIDECHAIN_EQ_MAX_GAIN_DB);
                        self.update_sidechain_eq(band);
                    }
                    "q" => {
                        self.sidechain_eq_q[band] =
                            value.clamp(SIDECHAIN_EQ_MIN_Q, SIDECHAIN_EQ_MAX_Q);
                        self.update_sidechain_eq(band);
                    }
                    _ => {}
                }
            }
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "threshold" => Some(self.threshold_db),
            "ratio" => Some(self.ratio),
            "ratio_plain" => Some(self.ratio_plain),
            "attack" => Some(self.attack_ms),
            "attack_plain" => Some(self.attack_plain),
            "release" => Some(self.release_ms),
            "release_plain" => Some(self.release_plain),
            "knee" => Some(self.knee_db),
            "range" => Some(self.range_db),
            "makeup" => Some(self.makeup_db),
            "lookahead" => Some(self.lookahead_ms),
            "max_lookahead" => Some(self.max_lookahead_ms),
            "detector_hpf" => Some(self.detector_hpf_hz),
            "stereo_link" => Some(self.stereo_link),
            "hold" => Some(self.hold_ms),
            "auto_release" => Some(if self.auto_release { 1.0 } else { 0.0 }),
            "detector" => Some(if self.detector_rms { 1.0 } else { 0.0 }),
            "mix" => Some(self.mix),
            "auto_makeup" => Some(if self.auto_makeup { 1.0 } else { 0.0 }),
            "mode" => Some(self.mode),
            _ => {
                let (band, field) = sidechain_eq_param(name)?;
                match field {
                    "on" => Some(if self.sidechain_eq_on[band] { 1.0 } else { 0.0 }),
                    "type" => Some(sidechain_eq_code(self.sidechain_eq_type[band])),
                    "frequency" => Some(self.sidechain_eq_frequency[band]),
                    "gain" => Some(self.sidechain_eq_gain[band]),
                    "q" => Some(self.sidechain_eq_q[band]),
                    _ => None,
                }
            }
        }
    }

    fn reset(&mut self) {
        let _ = self.acknowledge_processing_error();
        self.fast = [0.0; 2];
        self.slow = [0.0; 2];
        self.sustain_ms = [0.0; 2];
        self.hold_remaining = [0.0; 2];
        self.held_level = [0.0; 2];
        self.peak_hold = [0.0; 2];
        self.last_gr_db = 0.0;
        self.reset_delay();
        for filter in &mut self.hpf {
            filter.reset();
        }
        for filter in &mut self.sidechain_eq {
            filter.reset();
        }
    }

    fn latency_samples(&self) -> u32 {
        self.latency_frames() as u32
    }

    fn tail_samples(&self) -> u32 {
        self.latency_frames() as u32
    }

    fn gain_reduction_db(&self) -> f64 {
        self.last_gr_db
    }

    fn name(&self) -> &str {
        "Compressor"
    }
}

fn db_to_linear(db: f64) -> f64 {
    (db * std::f64::consts::LN_10 / 20.0).exp()
}

/// Split a detector-band parameter id into its band index and field name.
pub fn sidechain_eq_param(name: &str) -> Option<(usize, &'static str)> {
    let rest = name.strip_prefix("sidechain_eq")?;
    let (index, field) = rest.split_once('_')?;
    let index: usize = index.parse().ok()?;
    if !(1..=SIDECHAIN_EQ_BANDS).contains(&index) {
        return None;
    }
    let field = match field {
        "on" => "on",
        "type" => "type",
        "frequency" => "frequency",
        "gain" => "gain",
        "q" => "q",
        _ => return None,
    };
    Some((index - 1, field))
}

/// Detector-band shape for a schema code, in the device enum's option order.
/// A low cut is a high-pass and a high cut is a low-pass.
pub fn sidechain_eq_filter(code: f64) -> Option<FilterType> {
    match code.round() as i32 {
        0 => Some(FilterType::Peak),
        1 => Some(FilterType::LowShelf),
        2 => Some(FilterType::HighShelf),
        3 => Some(FilterType::HighPass),
        4 => Some(FilterType::LowPass),
        _ => None,
    }
}

fn sidechain_eq_code(filter: FilterType) -> f64 {
    match filter {
        FilterType::Peak => 0.0,
        FilterType::LowShelf => 1.0,
        FilterType::HighShelf => 2.0,
        FilterType::HighPass => 3.0,
        FilterType::LowPass => 4.0,
        _ => 0.0,
    }
}

/// Schema label to shape code, accepting the DSL's snake_case spellings.
pub fn sidechain_eq_type_code(name: &str) -> Option<f64> {
    let normalized = name.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    match normalized.as_str() {
        "bell" | "peak" => Some(0.0),
        "low_shelf" | "lowshelf" => Some(1.0),
        "high_shelf" | "highshelf" => Some(2.0),
        "low_cut" | "lowcut" | "high_pass" | "highpass" => Some(3.0),
        "high_cut" | "highcut" | "low_pass" | "lowpass" => Some(4.0),
        _ => None,
    }
}

fn linear_to_db(lin: f64) -> f64 {
    (20.0 * lin.max(1.0e-10).log10()).max(DB_FLOOR)
}

fn interp(table: &[(f64, f64)], x: f64) -> f64 {
    let x = x.clamp(table[0].0, table[table.len() - 1].0);
    for window in table.windows(2) {
        let (x0, y0) = window[0];
        let (x1, y1) = window[1];
        if x <= x1 {
            let t = if x1 > x0 { (x - x0) / (x1 - x0) } else { 0.0 };
            return y0 + t * (y1 - y0);
        }
    }
    table[table.len() - 1].1
}

fn interp_loglog(nodes: &[(f64, f64)], x: f64) -> f64 {
    let x = x.max(1.0e-4);
    let (x0, y0) = nodes[0];
    if x <= x0 {
        let (x1, y1) = nodes[1];
        let exponent = (y1 / y0).ln() / (x1 / x0).ln();
        return (y0 * (x / x0).powf(exponent)).max(1.0e-3);
    }
    for window in nodes.windows(2) {
        let (xa, ya) = window[0];
        let (xb, yb) = window[1];
        if x <= xb {
            let t = (x / xa).ln() / (xb / xa).ln();
            return (ya.ln() + t * (yb.ln() - ya.ln())).exp();
        }
    }
    let (xa, ya) = nodes[nodes.len() - 2];
    let (xb, yb) = nodes[nodes.len() - 1];
    let exponent = (yb / ya).ln() / (xb / xa).ln();
    (yb * (x / xb).powf(exponent)).max(1.0e-3)
}

pub(crate) fn ratio_plain_to_s(plain: f64) -> f64 {
    interp(&RATIO_CURVE, plain.clamp(0.0, 1.0))
}

fn ratio_to_plain(ratio: f64) -> f64 {
    let slope = (1.0 - 1.0 / ratio.max(1.0)).clamp(0.0, 1.0);
    for window in RATIO_CURVE.windows(2) {
        let (x0, y0) = window[0];
        let (x1, y1) = window[1];
        if slope <= y1 {
            return x0 + (x1 - x0) * (slope - y0) / (y1 - y0);
        }
    }
    1.0
}

fn plain_to_ratio(plain: f64) -> f64 {
    let slope = ratio_plain_to_s(plain);
    if slope >= 0.999_99 {
        100.0
    } else {
        1.0 / (1.0 - slope)
    }
}

pub(crate) fn attack_tau_ms(plain: f64) -> f64 {
    interp(&ATTACK_TAU_MS, plain.clamp(0.0, 1.0))
}

fn attack_plain_from_ms(ms: f64) -> f64 {
    let mut lo = 0.0;
    let mut hi = 1.0;
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if attack_tau_ms(mid) < ms {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

fn release_pair(plain: f64, long_hold: bool) -> (f64, f64) {
    let (fast_nodes, slow_nodes) = if long_hold {
        (
            [
                (RELEASE_LONG[0].0, RELEASE_LONG[0].1),
                (RELEASE_LONG[1].0, RELEASE_LONG[1].1),
                (RELEASE_LONG[2].0, RELEASE_LONG[2].1),
            ],
            [
                (RELEASE_LONG[0].0, RELEASE_LONG[0].2),
                (RELEASE_LONG[1].0, RELEASE_LONG[1].2),
                (RELEASE_LONG[2].0, RELEASE_LONG[2].2),
            ],
        )
    } else {
        (
            [
                (RELEASE_SHORT[0].0, RELEASE_SHORT[0].1),
                (RELEASE_SHORT[1].0, RELEASE_SHORT[1].1),
                (RELEASE_SHORT[2].0, RELEASE_SHORT[2].1),
            ],
            [
                (RELEASE_SHORT[0].0, RELEASE_SHORT[0].2),
                (RELEASE_SHORT[1].0, RELEASE_SHORT[1].2),
                (RELEASE_SHORT[2].0, RELEASE_SHORT[2].2),
            ],
        )
    };
    (
        interp_loglog(&fast_nodes, plain.clamp(0.0, 1.0)),
        interp_loglog(&slow_nodes, plain.clamp(0.0, 1.0)),
    )
}

fn release_plain_from_ms(ms: f64) -> f64 {
    let mut lo = 0.0;
    let mut hi = 1.0;
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if release_pair(mid, false).1 < ms {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[global_allocator]
    static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;

    const SR: u32 = 48_000;

    fn compressor(threshold: f64, plain: f64) -> Compressor {
        let mut c = Compressor::new(SR);
        c.set_param("threshold", threshold);
        c.set_param("ratio_plain", plain);
        c.set_param("attack_plain", 0.0);
        c.set_param("release_plain", 0.4);
        c.set_param("knee", 0.0);
        c
    }

    /// Sine starting at a crest so the detector sees the full step at sample 0.
    fn tone(amplitude: f64, ms: f64, mutes_after_ms: Option<f64>) -> Vec<(f64, f64)> {
        let frames = (SR as f64 * ms * 0.001) as usize;
        let cutoff = mutes_after_ms
            .map(|m| (SR as f64 * m * 0.001) as usize)
            .unwrap_or(frames);
        (0..frames)
            .map(|n| {
                let t = n as f64 / SR as f64;
                let sample = if n < cutoff {
                    amplitude * (std::f64::consts::TAU * 1000.0 * t).cos()
                } else {
                    0.0
                };
                (sample, sample)
            })
            .collect()
    }

    fn response(c: &mut Compressor, input: &[(f64, f64)]) -> Vec<f64> {
        let mut buffer = [0.0_f32; 2];
        let mut out = Vec::with_capacity(input.len());
        for &(l, r) in input {
            buffer[0] = l as f32;
            buffer[1] = r as f32;
            c.process(&mut buffer, SR);
            out.push(c.gain_reduction_db());
        }
        out
    }

    fn first_reaching(gr: &[f64], target: f64) -> Option<f64> {
        gr.iter()
            .position(|value| *value >= target)
            .map(|index| index as f64 * 1000.0 / SR as f64)
    }

    fn static_db(threshold: f64, plain: f64, input_db: f64) -> f64 {
        let amplitude = db_to_linear(input_db);
        let mut input = Vec::with_capacity(48_000);
        for n in 0..48_000 {
            let t = n as f64 / SR as f64;
            let sample = amplitude * (std::f64::consts::TAU * 1000.0 * t).cos();
            input.push((sample, sample));
        }
        let mut c = compressor(threshold, plain);
        let mut buffer = [0.0_f32; 2];
        let start = input.len() / 2;
        let mut input_peak: f64 = 0.0;
        let mut output_peak: f64 = 0.0;
        for (index, &(l, r)) in input.iter().enumerate() {
            buffer[0] = l as f32;
            buffer[1] = r as f32;
            c.process(&mut buffer, SR);
            if index >= start {
                input_peak = input_peak.max(l.abs());
                output_peak = output_peak.max((buffer[0] as f64).abs());
            }
        }
        20.0 * (input_peak / output_peak.max(1e-12)).log10()
    }

    fn fit_two_exponentials(times_ms: &[f64], values: &[f64]) -> (f64, f64, f64, f64) {
        let mut best = (0.0, 0.0, 0.0, 0.0, f64::INFINITY);
        for tau_fast in 5..=60 {
            for tau_slow in 40..=250 {
                let x1: Vec<f64> = times_ms
                    .iter()
                    .map(|t| (-t / tau_fast as f64).exp())
                    .collect();
                let x2: Vec<f64> = times_ms
                    .iter()
                    .map(|t| (-t / tau_slow as f64).exp())
                    .collect();
                let (mut a11, mut a12, mut a22, mut b1, mut b2) = (0.0, 0.0, 0.0, 0.0, 0.0);
                for ((x1, x2), value) in x1.iter().zip(&x2).zip(values) {
                    a11 += x1 * x1;
                    a12 += x1 * x2;
                    a22 += x2 * x2;
                    b1 += x1 * value;
                    b2 += x2 * value;
                }
                let det = a11 * a22 - a12 * a12;
                if det.abs() < 1e-12 {
                    continue;
                }
                let a = (b1 * a22 - b2 * a12) / det;
                let b = (b2 * a11 - b1 * a12) / det;
                if a <= 0.0 || b <= 0.0 {
                    continue;
                }
                let error: f64 = times_ms
                    .iter()
                    .zip(values)
                    .map(|(t, value)| {
                        let model =
                            a * (-t / tau_fast as f64).exp() + b * (-t / tau_slow as f64).exp();
                        (model - value).powi(2)
                    })
                    .sum();
                if error < best.4 {
                    best = (tau_fast as f64, tau_slow as f64, a, b, error);
                }
            }
        }
        (best.0, best.1, best.2, best.3)
    }

    fn release_fit(c: &mut Compressor, burst_ms: f64) -> (f64, f64, f64, f64, Vec<f64>) {
        let input = tone(db_to_linear(-6.0), 2000.0, Some(burst_ms));
        let start = (SR as f64 * burst_ms * 0.001) as usize;
        let gr = response(c, &input);
        let mut times = Vec::new();
        let mut values = Vec::new();
        for ms in [
            0.5, 1.0, 2.0, 3.0, 5.0, 8.0, 12.0, 20.0, 35.0, 50.0, 80.0, 120.0, 180.0, 260.0, 400.0,
        ] {
            let index = start + (SR as f64 * ms * 0.001) as usize;
            if index < gr.len() {
                times.push(ms);
                values.push(gr[index]);
            }
        }
        let (t1, t2, a, b) = fit_two_exponentials(&times, &values);
        (t1, t2, a, b, values)
    }

    #[test]
    fn ratio_curve_matches_the_measured_table() {
        for (plain, slope) in RATIO_CURVE {
            let measured = ratio_plain_to_s(plain);
            assert!(
                (measured - slope).abs() <= 0.0005,
                "plain {plain}: {measured} != {slope}"
            );
        }
        // The display labels are the DSP ratios at the reference points.
        for (plain, ratio) in [
            (0.1, 1.1),
            (0.2, 1.25),
            (0.3, 1.5),
            (0.4, 2.0),
            (0.5, 2.75),
            (0.6, 4.0),
            (0.7, 6.0),
            (0.8, 8.0),
            (0.9, 10.0),
            (0.95, 24.4),
            (0.98, 67.0),
        ] {
            let implied = 1.0 / (1.0 - ratio_plain_to_s(plain));
            assert!(
                (implied - ratio).abs() <= 0.03 * ratio,
                "plain {plain}: implied {implied} != {ratio}"
            );
        }
        assert_eq!(plain_to_ratio(1.0), 100.0);
        let mut c = Compressor::new(SR);
        c.set_param("ratio_plain", 0.6);
        assert!((c.get_param("ratio").unwrap() - 4.0).abs() < 1e-9);
        c.set_param("ratio", 3.0);
        assert!((c.get_param("ratio_plain").unwrap() - 0.5224).abs() < 0.002);
    }

    #[test]
    fn static_transfer_matches_the_measured_tables() {
        for (plain, slope) in [(0.3, 1.0 / 3.0), (0.6, 0.75), (0.85, 0.888_9)] {
            let c = compressor(-24.0, plain);
            for input in [-24.0, -21.0, -18.0, -15.0, -12.0, -9.0, -6.0, -3.0, 0.0] {
                let expected = slope * (input + 24.0);
                let actual = c.static_gr_db(input);
                assert!(
                    (actual - expected).abs() <= 0.01,
                    "plain {plain} input {input}: {actual} != {expected}"
                );
            }
        }
        // Knee 18 dB: the measured `0.75 * 9 * ((over + 9) / 18)^2` inside +-9 dB.
        let mut c = compressor(-24.0, 0.6);
        c.set_param("knee", 18.0);
        for (input, expected) in [
            (-33.0, 0.0),
            (-30.0, 0.19),
            (-27.0, 0.75),
            (-24.0, 1.69),
            (-21.0, 3.0),
            (-18.0, 4.69),
            (-15.0, 6.75),
            (-12.0, 9.0),
            (-9.0, 11.25),
            (-6.0, 13.5),
            (-3.0, 15.75),
            (0.0, 18.0),
        ] {
            let actual = c.static_gr_db(input);
            assert!(
                (actual - expected).abs() <= 0.01,
                "knee 18 input {input}: {actual} != {expected}"
            );
        }
    }

    #[test]
    fn knee_cap_is_72_db() {
        let mut c = compressor(-24.0, 0.6);
        c.set_param("knee", 72.0);
        assert_eq!(c.get_param("knee"), Some(72.0));
        // Inside a 72 dB knee the level is still warped 24 dB over threshold.
        let warped = c.static_gr_db(0.0);
        assert!((warped - 18.75).abs() < 0.01, "warped {warped}");
        // A 72 dB knee spans +-36 dB: at the threshold the warp is already 9 dB.
        assert!((c.static_gr_db(-24.0) - 6.75).abs() < 0.01);
    }

    #[test]
    fn range_is_a_hard_ceiling() {
        for ceiling in [3.0, 6.0, 12.0] {
            let mut c = compressor(-24.0, 0.6);
            c.set_param("range", ceiling);
            assert!((c.static_gr_db(-6.0) - ceiling).abs() < 1e-9);
            assert!((c.static_gr_db(0.0) - ceiling).abs() < 1e-9);
        }
        let c = compressor(-24.0, 0.6);
        assert!((c.static_gr_db(-6.0) - 13.5).abs() < 1e-9);
        assert!((c.static_gr_db(0.0) - 18.0).abs() < 1e-9);
    }

    #[test]
    fn auto_gain_is_one_over_e_not_one_half() {
        let mut c = compressor(-24.0, 0.6);
        assert!((c.auto_makeup_db() - 6.62).abs() < 0.02);
        let ratio = c.auto_makeup_db() / (0.75 * 24.0);
        assert!((ratio - 0.368).abs() <= 0.002, "constant {ratio}");
        // Linear in |threshold| and in the nominal slope.
        c.set_param("threshold", -12.0);
        assert!((c.auto_makeup_db() - 3.31).abs() < 0.02);
        c.set_param("threshold", -24.0);
        c.set_param("ratio_plain", 0.2);
        assert!((c.auto_makeup_db() - 1.77).abs() < 0.02);
    }

    #[test]
    fn attack_times_match_the_measured_points() {
        for (plain, reference_ms) in [(0.0, 0.77), (0.1, 1.19), (0.3, 11.08), (0.6, 106.21)] {
            let mut c = compressor(-24.0, 0.6);
            c.set_param("attack_plain", plain);
            let input = tone(db_to_linear(-6.0), 400.0, None);
            let gr = response(&mut c, &input);
            let reached = first_reaching(&gr, 10.0).expect("attack reaches +10 dB");
            let error = (reached - reference_ms).abs() / reference_ms;
            assert!(
                error <= 0.15,
                "attack plain {plain}: {reached} ms vs {reference_ms} ms ({:.1}%)",
                error * 100.0
            );
        }
    }

    #[test]
    fn release_is_two_stage_and_program_dependent() {
        let mut short = compressor(-24.0, 0.6);
        short.set_param("release_plain", 0.4);
        let (tau_fast, tau_slow, a, b, _) = release_fit(&mut short, 2.0);
        assert!(
            (tau_fast - 23.6).abs() / 23.6 <= 0.15,
            "fast tau {tau_fast}"
        );
        assert!(
            (tau_slow - 81.1).abs() / 81.1 <= 0.15,
            "slow tau {tau_slow}"
        );
        assert!(a > 2.5 && b > 6.0, "stages a={a} b={b}");

        let mut held = compressor(-24.0, 0.6);
        held.set_param("release_plain", 0.4);
        let (long_fast, long_slow, _, _, _) = release_fit(&mut held, 1000.0);
        assert!(
            (long_fast - 8.6).abs() / 8.6 <= 0.15,
            "held fast tau {long_fast}"
        );
        assert!(
            (long_slow - 147.3).abs() / 147.3 <= 0.15,
            "held slow tau {long_slow}"
        );
        assert!(
            long_fast < tau_fast && long_slow > tau_slow,
            "the two constants must move with hold time: {long_fast}/{long_slow} vs {tau_fast}/{tau_slow}"
        );
    }

    #[test]
    fn hold_freezes_the_reduction_for_at_least_450_ms() {
        let mut c = compressor(-24.0, 0.6);
        c.set_param("hold", 500.0);
        let input = tone(db_to_linear(-6.0), 900.0, Some(2.0));
        let gr = response(&mut c, &input);
        let start = (SR as f64 * 0.002) as usize;
        let at = |ms: f64| gr[start + (SR as f64 * ms * 0.001) as usize];
        let frozen = at(450.0);
        for ms in [10.0, 100.0, 250.0, 450.0] {
            assert!(
                (at(ms) - frozen).abs() < 0.02,
                "hold must freeze: {} at {ms} ms vs {frozen}",
                at(ms)
            );
        }
        assert!(at(700.0) < frozen - 1.0, "release resumes after the hold");
    }

    #[test]
    fn auto_release_slows_the_fast_stage() {
        let mut plain = compressor(-24.0, 0.6);
        plain.set_param("release_plain", 0.4);
        let (tau_plain, _, _, _, _) = release_fit(&mut plain, 2.0);
        let mut auto = compressor(-24.0, 0.6);
        auto.set_param("release_plain", 0.4);
        auto.set_param("auto_release", 1.0);
        let (tau_auto, _, _, _, _) = release_fit(&mut auto, 2.0);
        let ratio = tau_auto / tau_plain;
        assert!((1.25..=1.35).contains(&ratio), "auto release ratio {ratio}");
    }

    #[test]
    fn stereo_link_blends_towards_the_louder_channel() {
        let frames = (SR as f64 * 1.0) as usize;
        let (loud, quiet) = (db_to_linear(-12.0), db_to_linear(-60.0));
        let input: Vec<(f64, f64)> = (0..frames)
            .map(|n| {
                let phase = std::f64::consts::TAU * 1000.0 * n as f64 / SR as f64;
                (loud * phase.cos(), quiet * phase.cos())
            })
            .collect();
        for (link, expected_right) in [(0.0, 0.0), (0.5, 4.51), (1.0, 9.0)] {
            let mut c = compressor(-24.0, 0.6);
            c.set_param("stereo_link", link);
            let mut buffer = [0.0_f32; 2];
            let mut tail = Vec::new();
            for &(l, r) in &input {
                buffer[0] = l as f32;
                buffer[1] = r as f32;
                c.process(&mut buffer, SR);
                tail.push((buffer[0] as f64, buffer[1] as f64));
            }
            let n = tail.len();
            let left_peak = tail[n - 1000..]
                .iter()
                .map(|(l, _)| l.abs())
                .fold(0.0, f64::max);
            let right_peak = tail[n - 1000..]
                .iter()
                .map(|(_, r)| r.abs())
                .fold(0.0, f64::max);
            let left_gr = 20.0 * (loud / left_peak).log10();
            let right_gr = 20.0 * (quiet / right_peak.max(1e-12)).log10();
            assert!(
                (left_gr - 9.0).abs() < 0.15,
                "link {link}: left GR {left_gr}"
            );
            assert!(
                (right_gr - expected_right).abs() < 0.15,
                "link {link}: right GR {right_gr} != {expected_right}"
            );
        }
    }

    #[test]
    fn lookahead_is_a_fixed_line_and_the_knob_moves_the_tap() {
        let mut c = Compressor::new(SR);
        c.set_param("max_lookahead", 20.0);
        c.set_param("lookahead", 5.0);
        assert_eq!(c.latency_samples(), 960);
        let mut signal = vec![0.0_f32; 4096];
        signal[2000] = 0.5;
        signal[2001] = 0.5;
        c.process(&mut signal, SR);
        let onset = signal
            .chunks_exact(2)
            .position(|frame| frame[0].abs() > 1e-9)
            .expect("delayed impulse");
        assert_eq!(onset, 1000 + 960);
        c.set_param("lookahead", 20.0);
        assert_eq!(c.latency_samples(), 960);
        c.set_param("lookahead", 0.0);
        assert_eq!(c.latency_samples(), 0);
        c.set_param("max_lookahead", 0.0);
        c.set_param("lookahead", 5.0);
        assert_eq!(c.latency_samples(), 0);
    }

    #[test]
    fn below_threshold_lookahead_output_is_a_pure_delay() {
        let input: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.01).sin() * 0.01).collect();
        let mut delayed = input.clone();
        let mut c = Compressor::new(SR);
        c.set_param("max_lookahead", 20.0);
        c.set_param("lookahead", 5.0);
        c.process(&mut delayed, SR);
        let latency = 960 * 2;
        assert!(delayed[..latency].iter().all(|v| *v == 0.0));
        assert_eq!(&delayed[latency..], &input[..input.len() - latency]);
    }

    #[test]
    fn external_key_ducks_main_without_leaking_key_or_self_detection() {
        let input = vec![0.05_f32; 9600];
        let mut ducked = input.clone();
        let mut c = compressor(-24.0, 0.6);
        c.process_with_sidechain(&mut ducked, &vec![1.0; input.len()], SR, None);
        assert!(ducked[ducked.len() - 1] < 0.01);
        assert!(ducked.iter().all(|v| *v >= 0.0 && *v <= 0.05));
        c.reset();
        let mut silent_key = vec![0.9; input.len()];
        let expected = silent_key.clone();
        c.process_with_sidechain(&mut silent_key, &vec![0.0; input.len()], SR, None);
        assert_eq!(silent_key, expected);
        c.reset();
        let mut silent_main = vec![0.0; input.len()];
        c.process_with_sidechain(&mut silent_main, &vec![1.0; input.len()], SR, None);
        assert!(silent_main.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn self_key_is_exact_and_external_key_is_block_and_reset_invariant() {
        let input: Vec<f32> = (0..9600).map(|i| (i as f32 * 0.017).sin() * 0.6).collect();
        let mut plain = input.clone();
        compressor(-24.0, 0.6).process(&mut plain, SR);
        let mut keyed = input.clone();
        compressor(-24.0, 0.6).process_with_sidechain(&mut keyed, &input, SR, None);
        assert_eq!(plain, keyed);
        let mut split = input.clone();
        let mut c = compressor(-24.0, 0.6);
        for (audio, key) in split.chunks_mut(126).zip(input.chunks(126)) {
            c.process_with_sidechain(audio, key, SR, None);
        }
        assert_eq!(split, keyed);
        c.reset();
        split.copy_from_slice(&input);
        c.process_with_sidechain(&mut split, &input, SR, None);
        assert_eq!(split, keyed);
    }

    #[test]
    fn static_curve_tracks_the_measured_law_through_the_audio_path() {
        let c = compressor(-24.0, 0.6);
        for input_db in [-30.0, -24.0, -18.0, -12.0, -6.0, 0.0] {
            let expected = c.static_gr_db(input_db);
            let actual = static_db(-24.0, 0.6, input_db);
            assert!(
                (actual - expected).abs() <= 0.05,
                "input {input_db}: {actual} != {expected}"
            );
        }
    }

    #[test]
    fn auto_makeup_reaches_the_expected_static_gain() {
        let mut c = compressor(-24.0, 0.6);
        c.set_param("auto_makeup", 1.0);
        let input = tone(db_to_linear(-30.0), 1000.0, None);
        let mut buffer = [0.0_f32; 2];
        let mut peak: f64 = 0.0;
        for &(l, r) in &input {
            buffer[0] = l as f32;
            buffer[1] = r as f32;
            c.process(&mut buffer, SR);
            peak = peak.max((buffer[0] as f64).abs());
        }
        let measured = 20.0 * (peak / db_to_linear(-30.0)).log10();
        assert!((measured - 6.62).abs() < 0.05, "makeup {measured} dB");
    }

    #[test]
    fn mix_zero_is_bit_transparent() {
        let input: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.031).sin() * 0.9).collect();
        let mut output = input.clone();
        let mut c = Compressor::new(SR);
        c.set_param("mix", 0.0);
        c.process(&mut output, SR);
        assert_eq!(output, input);
    }

    #[test]
    fn detector_rms_is_accepted_but_always_sample_peak() {
        let input: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.017).sin() * 0.5).collect();
        let mut peak = input.clone();
        compressor(-24.0, 0.6).process(&mut peak, SR);
        let mut rms = input.clone();
        let mut c = compressor(-24.0, 0.6);
        c.set_param("detector", 1.0);
        c.process(&mut rms, SR);
        assert_eq!(peak, rms);
    }

    /// Output RMS of a steady tone after the detector has settled.
    fn steady_rms(frequency: f64, amplitude: f64, config: &[(&str, f64)]) -> f64 {
        let mut c = compressor(-30.0, 0.6);
        for (name, value) in config {
            c.set_param(name, *value);
        }
        let frames = 48_000;
        let start = frames / 2;
        let mut sum = 0.0;
        let mut buffer = [0.0_f32; 2];
        for n in 0..frames {
            let sample =
                amplitude * (std::f64::consts::TAU * frequency * n as f64 / SR as f64).sin();
            buffer[0] = sample as f32;
            buffer[1] = sample as f32;
            c.process(&mut buffer, SR);
            if n >= start {
                sum += (buffer[0] as f64).powi(2) + (buffer[1] as f64).powi(2);
            }
        }
        (sum / ((frames - start) * 2) as f64).sqrt()
    }

    fn db_ratio(a: f64, b: f64) -> f64 {
        20.0 * (a / b).log10()
    }

    #[test]
    fn detector_high_shelf_ducks_only_the_band_it_boosts() {
        let flat = -20.0_f64;
        let shelf = [
            ("sidechain_eq1_on", 1.0),
            ("sidechain_eq1_type", 2.0),
            ("sidechain_eq1_frequency", 6_000.0),
            ("sidechain_eq1_gain", 12.0),
            ("sidechain_eq1_q", 0.707),
        ];
        let body = amplitude(flat);
        let body_off = steady_rms(2_000.0, body, &[]);
        let body_on = steady_rms(2_000.0, body, &shelf);
        let sibilant = amplitude(flat);
        let sibilant_off = steady_rms(8_000.0, sibilant, &[]);
        let sibilant_on = steady_rms(8_000.0, sibilant, &shelf);
        println!(
            "2 kHz {:.5} -> {:.5} ({} dB); 8 kHz {:.5} -> {:.5} ({} dB)",
            body_off,
            body_on,
            db_ratio(body_on, body_off),
            sibilant_off,
            sibilant_on,
            db_ratio(sibilant_on, sibilant_off)
        );
        assert!(
            db_ratio(body_on, body_off).abs() < 0.15,
            "2 kHz moved by {} dB",
            db_ratio(body_on, body_off)
        );
        let extra = db_ratio(sibilant_on, sibilant_off);
        assert!(
            extra < -6.0,
            "8 kHz gained only {extra} dB of detection; the shelf did not reach the detector"
        );
    }

    #[test]
    fn detector_low_cut_removes_low_frequency_detection() {
        let low_cut = [
            ("sidechain_eq1_on", 1.0),
            ("sidechain_eq1_type", 3.0),
            ("sidechain_eq1_frequency", 400.0),
            ("sidechain_eq1_gain", 0.0),
            ("sidechain_eq1_q", 0.707),
        ];
        let level = amplitude(-20.0);
        let low_off = steady_rms(60.0, level, &[]);
        let low_on = steady_rms(60.0, level, &low_cut);
        let high_off = steady_rms(4_000.0, level, &[]);
        let high_on = steady_rms(4_000.0, level, &low_cut);
        println!(
            "60 Hz {} dB, 4 kHz {} dB",
            db_ratio(low_on, low_off),
            db_ratio(high_on, high_off)
        );
        assert!(
            db_ratio(low_on, low_off) > 5.0,
            "60 Hz is still detected: {} dB",
            db_ratio(low_on, low_off)
        );
        assert!(
            db_ratio(high_on, high_off).abs() < 1.0,
            "4 kHz detector level moved by {} dB",
            db_ratio(high_on, high_off)
        );
    }

    #[test]
    fn detector_eq_never_reaches_the_audio_path() {
        let quiet: Vec<f32> = (0..4800)
            .map(|i| (i as f32 * 0.013).sin() * 0.001)
            .collect();
        let mut output = quiet.clone();
        let mut c = Compressor::new(SR);
        c.set_param("sidechain_eq1_on", 1.0);
        c.set_param("sidechain_eq1_type", 4.0);
        c.set_param("sidechain_eq1_frequency", 1_500.0);
        c.set_param("sidechain_eq1_gain", -30.0);
        c.set_param("sidechain_eq2_on", 1.0);
        c.set_param("sidechain_eq2_type", 0.0);
        c.set_param("sidechain_eq2_frequency", 500.0);
        c.set_param("sidechain_eq2_gain", 24.0);
        c.process(&mut output, SR);
        assert_eq!(output, quiet);
    }

    #[test]
    fn detector_eq_filters_an_external_key() {
        let shelf = [
            ("sidechain_eq1_on", 1.0),
            ("sidechain_eq1_type", 2.0),
            ("sidechain_eq1_frequency", 6_000.0),
            ("sidechain_eq1_gain", 12.0),
        ];
        // An 8 kHz key sits under a -24 dB threshold; the shelf lifts the key
        // the detector reads, so an external trigger must reduce more.
        let frames = 9_600;
        let key: Vec<f32> = (0..frames * 2)
            .map(|i| {
                let n = i / 2;
                (std::f64::consts::TAU * 8_000.0 * n as f64 / SR as f64).sin() as f32 * 0.1
            })
            .collect();
        let mut plain = key.clone();
        compressor(-24.0, 0.6).process_with_sidechain(&mut plain, &key, SR, None);
        let mut c = compressor(-24.0, 0.6);
        for (name, value) in shelf {
            c.set_param(name, value);
        }
        let mut shaped = key.clone();
        c.process_with_sidechain(&mut shaped, &key, SR, None);
        let reduction = db_ratio(rms_of(&shaped), rms_of(&plain));
        println!("external 8 kHz key with the shelf: {reduction:.2} dB");
        assert!(
            reduction < -6.0,
            "the key shelf changed the reduction by only {reduction} dB"
        );
    }

    fn rms_of(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    fn amplitude(db: f64) -> f64 {
        db_to_linear(db)
    }

    fn fnv1a(samples: &[f32]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for sample in samples {
            for byte in sample.to_bits().to_le_bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    #[test]
    fn defaults_render_the_pre_detector_eq_digest() {
        // Pinned from the device before the detector EQ existed: the default
        // audio path must not move.
        let frames = 24_000;
        let mut buffer = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let t = n as f64 / SR as f64;
            let left = 0.55 * (std::f64::consts::TAU * 110.0 * t).sin()
                + 0.25 * (std::f64::consts::TAU * 1_000.0 * t).sin()
                + 0.15 * (std::f64::consts::TAU * 7_000.0 * t + 0.4).sin();
            let right = 0.45 * (std::f64::consts::TAU * 220.0 * t + 0.9).sin()
                + 0.2 * (std::f64::consts::TAU * 3_000.0 * t).sin();
            buffer.push(left as f32);
            buffer.push(right as f32);
        }
        let mut c = Compressor::new(SR);
        c.process(&mut buffer, SR);
        assert_eq!(fnv1a(&buffer), 0xfe20_2270_980a_ccd9);
    }

    fn prepared_device(rate: u32) -> Compressor {
        let mut device = Compressor::new(rate);
        device.set_param("lookahead", 5.0);
        device.set_param("detector_hpf", 80.0);
        device.set_param("sidechain_eq1_on", 1.0);
        device.set_param("sidechain_eq1_frequency", 800.0);
        device.set_param("sidechain_eq1_gain", 2.0);
        device
    }

    #[test]
    fn prepared_compressor_control_reads_directly_from_first_and_growing_blocks_without_heap() {
        for rate in [8_000, 44_100, 48_000, 96_000] {
            let mut actual = prepared_device(rate);
            let mut reference = prepared_device(rate);
            for frames in [1usize, 0, 31, 128, 4096] {
                let mut audio: Vec<f32> = (0..frames * 2)
                    .map(|index| ((index / 2) as f32 * 0.17).sin() * 0.8)
                    .collect();
                let mut expected = audio.clone();
                let control: Vec<f32> = (0..frames.saturating_sub(3))
                    .map(|index| match index % 7 {
                        0 => f32::NAN,
                        1 => f32::INFINITY,
                        2 => -0.2,
                        3 => 1.5,
                        _ => 0.7,
                    })
                    .collect();
                // Reference is the previous materialized stereo-key contract,
                // including non-finite control as silence and a short tail.
                let key: Vec<f32> = (0..frames)
                    .flat_map(|index| {
                        let value = control.get(index).copied().unwrap_or(0.0);
                        let level = if value.is_finite() {
                            value.clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        [level, level]
                    })
                    .collect();
                reference
                    .try_process_with_sidechain(&mut expected, &key, rate)
                    .unwrap();
                assert_eq!(
                    oc_test_support::operations(|| actual
                        .try_process_with_control(&mut audio, &control, rate)
                        .unwrap()),
                    0,
                    "rate={rate}, frames={frames}; first control call is measured without warmup or preparation"
                );
                assert_eq!(audio, expected);
                assert_eq!(actual.gain_reduction_db(), reference.gain_reduction_db());
                assert_eq!(actual.latency_samples(), reference.latency_samples());
            }
        }
    }

    #[test]
    fn prepared_compressor_self_short_key_and_reset_match_contiguous_reference_without_heap() {
        for rate in [8_000, 48_000, 96_000] {
            let mut actual = prepared_device(rate);
            let mut reference = prepared_device(rate);
            let mut keyed = prepared_device(rate);
            let input: Vec<f32> = (0..8192)
                .map(|index| ((index / 2) as f32 * 0.07).cos() * 0.8)
                .collect();
            let mut audio = input.clone();
            let mut expected = input.clone();
            reference
                .try_process_with_sidechain(&mut expected, &input, rate)
                .unwrap();
            assert_eq!(
                oc_test_support::operations(|| actual.try_process(&mut audio, rate).unwrap()),
                0
            );
            assert_eq!(audio, expected);
            let short_key = &input[..62];
            let mut padded = vec![0.0; input.len()];
            padded[..short_key.len()].copy_from_slice(short_key);
            let mut short = input.clone();
            let mut full = input.clone();
            let mut full_device = prepared_device(rate);
            full_device
                .try_process_with_sidechain(&mut full, &padded, rate)
                .unwrap();
            assert_eq!(
                oc_test_support::operations(|| keyed
                    .try_process_with_sidechain(&mut short, short_key, rate)
                    .unwrap()),
                0
            );
            assert_eq!(short, full);
            // A reset clears all history but retains all load-time storage.
            audio.copy_from_slice(&input);
            let mut fresh = prepared_device(rate);
            let mut fresh_audio = input.clone();
            fresh.try_process(&mut fresh_audio, rate).unwrap();
            assert_eq!(
                oc_test_support::operations(|| {
                    actual.reset();
                    actual.try_process(&mut audio, rate).unwrap();
                }),
                0
            );
            assert_eq!(audio, fresh_audio);
            assert_eq!(actual.gain_reduction_db(), fresh.gain_reduction_db());
            // Different call boundaries must not alter the detector or delay.
            let mut partial = prepared_device(rate);
            let mut split = input.clone();
            assert_eq!(
                oc_test_support::operations(|| {
                    for block in split.chunks_mut(62) {
                        partial.try_process(block, rate).unwrap();
                    }
                }),
                0
            );
            assert_eq!(split, fresh_audio);
        }
    }

    #[test]
    fn prepared_compressor_malformed_rate_audio_and_key_refuse_without_partial_dsp() {
        let mut actual = prepared_device(SR);
        let mut reference = prepared_device(SR);
        for error in [
            CompressorProcessError::IncompleteFrame,
            CompressorProcessError::IncompleteSidechainFrame,
            CompressorProcessError::NonFiniteAudio,
            CompressorProcessError::NonFiniteSidechain,
            CompressorProcessError::SampleRateChanged,
        ] {
            let mut audio = [0.8; 32];
            let mut key = [0.7; 32];
            if error == CompressorProcessError::NonFiniteAudio {
                audio[31] = f32::NAN;
            }
            if error == CompressorProcessError::NonFiniteSidechain {
                key[31] = f32::INFINITY;
            }
            let before = audio.map(f32::to_bits);
            assert_eq!(
                oc_test_support::operations(|| {
                    let result = match error {
                        CompressorProcessError::IncompleteFrame => {
                            actual.try_process(&mut audio[..31], SR)
                        }
                        CompressorProcessError::IncompleteSidechainFrame => {
                            actual.try_process_with_sidechain(&mut audio, &key[..31], SR)
                        }
                        CompressorProcessError::NonFiniteAudio => {
                            actual.try_process(&mut audio, SR)
                        }
                        CompressorProcessError::NonFiniteSidechain => {
                            actual.try_process_with_sidechain(&mut audio, &key, SR)
                        }
                        CompressorProcessError::SampleRateChanged => {
                            actual.try_process(&mut audio, 44_100)
                        }
                    };
                    assert_eq!(result, Err(error));
                    assert!(actual.processing_failed());
                    assert_eq!(actual.acknowledge_processing_error(), Some(error));
                }),
                0
            );
            assert_eq!(audio.map(f32::to_bits), before);
            let mut next = [0.8; 512];
            let mut expected = next;
            reference.try_process(&mut expected, SR).unwrap();
            actual.try_process(&mut next, SR).unwrap();
            assert_eq!(
                next, expected,
                "refused block must not advance filters, detector or lookahead"
            );
        }
        let mut invalid = [f32::INFINITY, 0.0];
        assert_eq!(
            oc_test_support::operations(|| actual.process(&mut invalid, SR)),
            0
        );
        assert!(matches!(
            actual.take_processing_failure(),
            oc_audio::ProcessingFailureReport::Ready(_)
        ));
        assert_eq!(
            actual.take_processing_failure(),
            oc_audio::ProcessingFailureReport::Reported
        );
        actual.reset();
        assert!(!actual.processing_failed());
    }

    #[test]
    fn prepared_compressor_realtime_parameters_and_preparation_retain_coefficients_and_heap() {
        let mut actual = prepared_device(SR);
        let values = [
            ("threshold", -18.0),
            ("ratio", 6.0),
            ("ratio_plain", 0.6),
            ("attack", 5.0),
            ("attack_plain", 0.3),
            ("release", 80.0),
            ("release_plain", 0.4),
            ("auto_release", 1.0),
            ("detector_hpf", 120.0),
            ("sidechain_eq1_on", 1.0),
            ("sidechain_eq1_type", 2.0),
            ("sidechain_eq1_frequency", 400.0),
            ("sidechain_eq1_gain", 3.0),
            ("sidechain_eq1_q", 1.0),
            ("lookahead", 5.0),
            ("max_lookahead", 10.0),
        ];
        assert_eq!(
            oc_test_support::operations(|| {
                actual.prepare_block(4096).unwrap();
                assert_eq!(
                    actual.prepare_block(usize::MAX),
                    Err(oc_audio::BlockPreparationError::CapacityExceeded)
                );
                for (name, value) in values {
                    actual.set_param(name, value);
                }
                for (name, _) in values {
                    let before = actual.get_param(name);
                    actual.set_param(name, f64::NAN);
                    actual.set_param(name, f64::INFINITY);
                    assert_eq!(actual.get_param(name), before);
                }
            }),
            0
        );
        let release = (actual.fast_coeffs, actual.slow_coeffs);
        actual.set_param("attack_plain", 0.7);
        assert_eq!(
            (actual.fast_coeffs, actual.slow_coeffs),
            release,
            "attack edits do not rebuild release tables"
        );
        let attack = actual.attack_coeff;
        actual.set_param("release_plain", 0.7);
        assert_eq!(
            actual.attack_coeff, attack,
            "release edits do not rebuild attack coefficients"
        );
        assert_eq!(
            actual.compression_slope,
            ratio_plain_to_s(actual.ratio_plain)
        );
        let mut audio = [0.8; 256];
        assert_eq!(
            oc_test_support::operations(|| actual
                .try_process_with_control(&mut audio, &[0.7; 128], SR)
                .unwrap()),
            0
        );
    }
}
