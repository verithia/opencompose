//! Dynamic EQ with serial parametric bands and dedicated low/high cuts.
//!
//! Bands run in series as RBJ biquads. Each dynamic band detects its own
//! filtered output, or an external key's band-filtered input when its
//! `bandN_sidechain` switch is on. Auto-gain measures K-weighted input and
//! output power and applies a matching makeup so loudness stays neutral.

use super::AudioEffect;
use super::biquad::{BiquadCoefficients, FilterType};
use super::kweight::KWeightChannel;
use super::oversampler::fir::{FIR_2X_LATENCY, Fir2x};
use super::oversampler::polyphase::Polyphase2x;
use oc_device_schema::{DYNAMIC_EQ_CENTER_FREQUENCIES, DYNAMIC_EQ_OUTPUT_GAIN_DB_RANGE};

mod cut_slope;
use cut_slope::{CutSlope, ExtraSections, FilterState};

/// Number of dynamic EQ bands.
pub const BAND_COUNT: usize = DYNAMIC_EQ_CENTER_FREQUENCIES.len();
/// Lowest band frequency.
pub const MIN_FREQ: f64 = 20.0;
/// Highest band frequency.
pub const MAX_FREQ: f64 = 20_000.0;
const FILTER_RATE_CEILING: f64 = 0.49;
/// Lowest band Q.
pub const MIN_Q: f64 = 0.1;
/// Highest band Q.
pub const MAX_Q: f64 = 20.0;
/// Lowest static band gain.
pub const MIN_GAIN_DB: f64 = -24.0;
/// Highest static band gain.
pub const MAX_GAIN_DB: f64 = 24.0;
/// Lowest detector threshold.
pub const MIN_THRESHOLD_DB: f64 = -60.0;
/// Highest detector threshold.
pub const MAX_THRESHOLD_DB: f64 = 0.0;
/// Lowest dynamic gain range.
pub const MIN_RANGE_DB: f64 = -24.0;
/// Highest dynamic gain range.
pub const MAX_RANGE_DB: f64 = 24.0;
/// Fastest detector attack.
pub const MIN_ATTACK_MS: f64 = 0.1;
/// Slowest detector attack.
pub const MAX_ATTACK_MS: f64 = 200.0;
/// Fastest detector release.
pub const MIN_RELEASE_MS: f64 = 5.0;
/// Slowest detector release.
pub const MAX_RELEASE_MS: f64 = 2000.0;

/// Samples between coefficient updates. Envelope detection stays per-sample.
const CONTROL_INTERVAL: u32 = 16;
/// Gain reaches `range` once the detector is 12 dB above threshold.
const DYNAMIC_SPAN_DB: f64 = 12.0;
const MIN_ENVELOPE: f64 = 1e-30;
/// Auto-gain loudness integration window and gain smoothing, in milliseconds.
const AUTO_GAIN_WINDOW_MS: f64 = 400.0;
const AUTO_GAIN_SMOOTH_MS: f64 = 150.0;
const AUTO_GAIN_MAX_DB: f64 = 24.0;
const MIN_AUTO_POWER: f64 = 1e-20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Filter shape of one dynamic EQ band.
pub enum BandKind {
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
    BandPass,
}

impl BandKind {
    /// Map a band-type label to its kind.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "bell" | "peak" | "peaking" => Some(Self::Bell),
            "low_shelf" | "lowshelf" => Some(Self::LowShelf),
            "high_shelf" | "highshelf" => Some(Self::HighShelf),
            "low_cut" | "lowcut" | "high_pass" => Some(Self::LowCut),
            "high_cut" | "highcut" | "low_pass" => Some(Self::HighCut),
            "notch" => Some(Self::Notch),
            "band_pass" | "bandpass" => Some(Self::BandPass),
            _ => None,
        }
    }

    pub(crate) fn from_code(code: f64) -> Option<Self> {
        match code.round() as i32 {
            0 => Some(Self::Bell),
            1 => Some(Self::LowShelf),
            2 => Some(Self::HighShelf),
            3 => Some(Self::LowCut),
            4 => Some(Self::HighCut),
            5 => Some(Self::Notch),
            6 => Some(Self::BandPass),
            _ => None,
        }
    }

    /// The selector code stored in a preset.
    pub fn code(self) -> f64 {
        match self {
            Self::Bell => 0.0,
            Self::LowShelf => 1.0,
            Self::HighShelf => 2.0,
            Self::LowCut => 3.0,
            Self::HighCut => 4.0,
            Self::Notch => 5.0,
            Self::BandPass => 6.0,
        }
    }

    fn filter_type(self) -> FilterType {
        match self {
            Self::Bell => FilterType::Peak,
            Self::LowShelf => FilterType::LowShelf,
            Self::HighShelf => FilterType::HighShelf,
            Self::LowCut => FilterType::HighPass,
            Self::HighCut => FilterType::LowPass,
            Self::Notch => FilterType::Notch,
            Self::BandPass => FilterType::BandPass,
        }
    }
}

/// Which channel(s) a band processes. Mid/side work on the M/S components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelTarget {
    Stereo,
    Mid,
    Side,
    Left,
    Right,
}

impl ChannelTarget {
    /// Map a channel label to its target.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "stereo" => Some(Self::Stereo),
            "mid" | "m" => Some(Self::Mid),
            "side" | "s" => Some(Self::Side),
            "left" | "l" => Some(Self::Left),
            "right" | "r" => Some(Self::Right),
            _ => None,
        }
    }

    pub(crate) fn from_code(code: f64) -> Option<Self> {
        match code.round() as i32 {
            0 => Some(Self::Stereo),
            1 => Some(Self::Mid),
            2 => Some(Self::Side),
            3 => Some(Self::Left),
            4 => Some(Self::Right),
            _ => None,
        }
    }

    /// The selector code stored in a preset.
    pub fn code(self) -> f64 {
        match self {
            Self::Stereo => 0.0,
            Self::Mid => 1.0,
            Self::Side => 2.0,
            Self::Left => 3.0,
            Self::Right => 4.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DynamicBand {
    enabled: bool,
    solo: bool,
    channel_target: ChannelTarget,
    kind: BandKind,
    slope: CutSlope,
    freq: f64,
    q: f64,
    gain_db: f64,
    dynamic: bool,
    sidechain: bool,
    threshold_db: f64,
    range_db: f64,
    attack_ms: f64,
    release_ms: f64,
    attack_coeff: f64,
    release_coeff: f64,
    threshold_lin: f64,
    full_scale_lin: f64,
    sample_rate: f64,
    envelope: f64,
    offset_db: f64,
    coefficients: BiquadCoefficients,
    extra_sections: ExtraSections,
    target: BiquadCoefficients,
    step: BiquadCoefficients,
    states: [FilterState; 2],
    side_states: [FilterState; 2],
    designed_gain_db: f64,
    control_countdown: u32,
    dirty: bool,
}

impl DynamicBand {
    fn new(index: usize, sample_rate: u32) -> Self {
        let freq = DYNAMIC_EQ_CENTER_FREQUENCIES[index];
        let mut band = Self {
            enabled: false,
            solo: false,
            channel_target: ChannelTarget::Stereo,
            kind: BandKind::Bell,
            slope: CutSlope::default(),
            freq,
            q: 1.0,
            gain_db: 0.0,
            dynamic: false,
            sidechain: false,
            threshold_db: -30.0,
            range_db: -6.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            threshold_lin: 0.0,
            full_scale_lin: 0.0,
            sample_rate: sample_rate as f64,
            envelope: 0.0,
            offset_db: 0.0,
            coefficients: BiquadCoefficients::default(),
            extra_sections: ExtraSections::default(),
            target: BiquadCoefficients::default(),
            step: BiquadCoefficients::default(),
            states: [FilterState::default(); 2],
            side_states: [FilterState::default(); 2],
            designed_gain_db: 0.0,
            control_countdown: 0,
            dirty: true,
        };
        band.update_detector();
        band
    }

    fn reset_runtime(&mut self) {
        self.states = [FilterState::default(); 2];
        self.reset_detector();
        self.control_countdown = 0;
    }

    fn reset_detector(&mut self) {
        self.side_states = [FilterState::default(); 2];
        self.envelope = 0.0;
        self.offset_db = 0.0;
    }

    fn update_detector(&mut self) {
        self.attack_coeff = time_coefficient(self.attack_ms, self.sample_rate);
        self.release_coeff = time_coefficient(self.release_ms, self.sample_rate);
        self.threshold_lin = db_to_linear(self.threshold_db);
        self.full_scale_lin = self.threshold_lin * db_to_linear(DYNAMIC_SPAN_DB);
    }

    fn prepare(&mut self, sample_rate: f64) {
        if (self.sample_rate - sample_rate).abs() > 0.5 {
            self.sample_rate = sample_rate;
            self.update_detector();
            self.reset_runtime();
            self.dirty = true;
        }
        if !self.dirty {
            return;
        }
        // Retuning keeps the delay and detector states, including when
        // automation supplies a different cutoff at every sample.
        let gain_db = (self.gain_db + self.offset_db).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
        (self.coefficients, self.extra_sections) = ExtraSections::design(
            self.kind.filter_type(),
            self.freq,
            self.q,
            gain_db,
            sample_rate,
            self.slope,
        );
        self.target = self.coefficients;
        self.designed_gain_db = gain_db;
        self.control_countdown = 0;
        self.dirty = false;
    }

    fn set_field(&mut self, field: &str, value: f64) -> bool {
        match field {
            "enabled" => {
                let enabled = value >= 0.5;
                if enabled == self.enabled {
                    return false;
                }
                self.enabled = enabled;
                self.reset_runtime();
                true
            }
            "solo" => {
                let solo = value >= 0.5;
                if solo == self.solo {
                    return false;
                }
                self.solo = solo;
                true
            }
            "channel" => {
                let Some(target) = ChannelTarget::from_code(value) else {
                    return false;
                };
                if target == self.channel_target {
                    return false;
                }
                self.channel_target = target;
                self.reset_runtime();
                true
            }
            "type" => {
                let Some(kind) = BandKind::from_code(value) else {
                    return false;
                };
                if kind == self.kind {
                    return false;
                }
                self.kind = kind;
                self.reset_runtime();
                true
            }
            "slope" => {
                let Some(slope) = CutSlope::from_parameter(value) else {
                    return false;
                };
                if self.slope == slope {
                    return false;
                }
                self.slope = slope;
                if matches!(self.kind, BandKind::LowCut | BandKind::HighCut) {
                    self.reset_runtime();
                    return true;
                }
                false
            }
            "freq" => {
                let freq = value.clamp(MIN_FREQ, MAX_FREQ);
                if freq != self.freq {
                    self.freq = freq;
                    return true;
                }
                false
            }
            "q" => {
                let q = value.clamp(MIN_Q, MAX_Q);
                if q != self.q {
                    self.q = q;
                    return true;
                }
                false
            }
            "gain" => {
                let gain = value.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
                if gain != self.gain_db {
                    self.gain_db = gain;
                    return true;
                }
                false
            }
            "dynamic" => {
                let dynamic = value >= 0.5;
                if dynamic == self.dynamic {
                    return false;
                }
                self.dynamic = dynamic;
                self.reset_detector();
                true
            }
            "sidechain" => {
                let sidechain = value >= 0.5;
                if sidechain == self.sidechain {
                    return false;
                }
                self.sidechain = sidechain;
                self.reset_detector();
                true
            }
            "threshold" => {
                let threshold = value.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB);
                if threshold == self.threshold_db {
                    return false;
                }
                self.threshold_db = threshold;
                self.update_detector();
                true
            }
            "range" => {
                let range = value.clamp(MIN_RANGE_DB, MAX_RANGE_DB);
                if range == self.range_db {
                    return false;
                }
                self.range_db = range;
                true
            }
            "attack" => {
                let attack = value.clamp(MIN_ATTACK_MS, MAX_ATTACK_MS);
                if attack == self.attack_ms {
                    return false;
                }
                self.attack_ms = attack;
                self.update_detector();
                true
            }
            "release" => {
                let release = value.clamp(MIN_RELEASE_MS, MAX_RELEASE_MS);
                if release == self.release_ms {
                    return false;
                }
                self.release_ms = release;
                self.update_detector();
                true
            }
            _ => false,
        }
    }

    fn get_field(&self, field: &str) -> Option<f64> {
        match field {
            "enabled" => Some(if self.enabled { 1.0 } else { 0.0 }),
            "solo" => Some(if self.solo { 1.0 } else { 0.0 }),
            "channel" => Some(self.channel_target.code()),
            "type" => Some(self.kind.code()),
            "slope" => Some(self.slope.parameter()),
            "freq" => Some(self.freq),
            "q" => Some(self.q),
            "gain" => Some(self.gain_db),
            "dynamic" => Some(if self.dynamic { 1.0 } else { 0.0 }),
            "sidechain" => Some(if self.sidechain { 1.0 } else { 0.0 }),
            "threshold" => Some(self.threshold_db),
            "range" => Some(self.range_db),
            "attack" => Some(self.attack_ms),
            "release" => Some(self.release_ms),
            _ => None,
        }
    }

    /// Filter the external key with the band's current shape so the detector
    /// stays band-selective and aligned with self detection.
    fn detector_frame(&mut self, left: f64, right: f64) -> (f64, f64) {
        match self.channel_target {
            ChannelTarget::Stereo => (
                self.side_states[0].process(&self.coefficients, &self.extra_sections, left),
                self.side_states[1].process(&self.coefficients, &self.extra_sections, right),
            ),
            ChannelTarget::Left => {
                let out =
                    self.side_states[0].process(&self.coefficients, &self.extra_sections, left);
                (out, out)
            }
            ChannelTarget::Right => {
                let out =
                    self.side_states[0].process(&self.coefficients, &self.extra_sections, right);
                (out, out)
            }
            ChannelTarget::Mid => {
                let mid = (left + right) * 0.5;
                let out =
                    self.side_states[0].process(&self.coefficients, &self.extra_sections, mid);
                (out, out)
            }
            ChannelTarget::Side => {
                let side = (left - right) * 0.5;
                let out =
                    self.side_states[0].process(&self.coefficients, &self.extra_sections, side);
                (out, out)
            }
        }
    }

    fn process_frame(&mut self, left: f64, right: f64, key: Option<(f64, f64)>) -> (f64, f64) {
        let (next_left, next_right, detected) = match self.channel_target {
            ChannelTarget::Stereo => {
                let left_out =
                    self.states[0].process(&self.coefficients, &self.extra_sections, left);
                let right_out =
                    self.states[1].process(&self.coefficients, &self.extra_sections, right);
                (left_out, right_out, (left_out, right_out))
            }
            ChannelTarget::Left => {
                let out = self.states[0].process(&self.coefficients, &self.extra_sections, left);
                (out, right, (out, out))
            }
            ChannelTarget::Right => {
                let out = self.states[0].process(&self.coefficients, &self.extra_sections, right);
                (left, out, (out, out))
            }
            ChannelTarget::Mid => {
                let mid = (left + right) * 0.5;
                let side = (left - right) * 0.5;
                let out = self.states[0].process(&self.coefficients, &self.extra_sections, mid);
                (out + side, out - side, (out, out))
            }
            ChannelTarget::Side => {
                let mid = (left + right) * 0.5;
                let side = (left - right) * 0.5;
                let out = self.states[0].process(&self.coefficients, &self.extra_sections, side);
                (mid + out, mid - out, (out, out))
            }
        };
        if !self.dynamic {
            return (next_left, next_right);
        }
        let detector = match key {
            Some((key_left, key_right)) if self.sidechain => {
                self.detector_frame(key_left, key_right)
            }
            _ => detected,
        };
        self.process_dynamic(detector.0, detector.1, self.sample_rate);
        (next_left, next_right)
    }

    fn process_dynamic(&mut self, left: f64, right: f64, sample_rate: f64) {
        let peak = left.abs().max(right.abs());
        let coefficient = if peak > self.envelope {
            self.attack_coeff
        } else {
            self.release_coeff
        };
        self.envelope = coefficient * self.envelope + (1.0 - coefficient) * peak;
        if self.envelope < MIN_ENVELOPE {
            self.envelope = 0.0;
        }

        let span = self.full_scale_lin - self.threshold_lin;
        let amount = if span > 0.0 {
            ((self.envelope - self.threshold_lin) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.offset_db = self.range_db * amount;

        // Cut gain does not change with the detector. Its first section keeps
        // the Q chosen for the full cascade.
        if matches!(self.kind, BandKind::LowCut | BandKind::HighCut) {
            return;
        }

        if self.control_countdown == 0 {
            let total = (self.gain_db + self.offset_db).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
            if (total - self.designed_gain_db).abs() > 1e-9 {
                self.target = BiquadCoefficients::design_with_ceiling(
                    self.kind.filter_type(),
                    self.freq,
                    self.q,
                    total,
                    sample_rate,
                    FILTER_RATE_CEILING,
                );
                self.designed_gain_db = total;
            }
            self.step = self
                .coefficients
                .step_toward(&self.target, CONTROL_INTERVAL as f64);
            self.control_countdown = CONTROL_INTERVAL;
        }
        self.control_countdown -= 1;
        self.coefficients.advance(&self.step);
    }
}

#[derive(Debug, Clone, Copy)]
struct Cut {
    enabled: bool,
    freq: f64,
    q: f64,
    filter_type: FilterType,
    slope: CutSlope,
    coefficients: BiquadCoefficients,
    extra_sections: ExtraSections,
    states: [FilterState; 2],
    dirty: bool,
}

impl Cut {
    fn new(filter_type: FilterType, freq: f64, sample_rate: u32) -> Self {
        Self {
            enabled: false,
            freq,
            q: 0.707,
            filter_type,
            slope: CutSlope::default(),
            coefficients: BiquadCoefficients::design_with_ceiling(
                filter_type,
                freq,
                0.707,
                0.0,
                sample_rate as f64,
                FILTER_RATE_CEILING,
            ),
            extra_sections: ExtraSections::default(),
            states: [FilterState::default(); 2],
            dirty: false,
        }
    }

    fn prepare(&mut self, sample_rate: f64) {
        if self.dirty {
            (self.coefficients, self.extra_sections) = ExtraSections::design(
                self.filter_type,
                self.freq,
                self.q,
                0.0,
                sample_rate,
                self.slope,
            );
            self.dirty = false;
        }
    }

    fn process_pair(&mut self, left: f64, right: f64) -> (f64, f64) {
        (
            self.states[0].process(&self.coefficients, &self.extra_sections, left),
            self.states[1].process(&self.coefficients, &self.extra_sections, right),
        )
    }

    fn reset(&mut self) {
        self.states = [FilterState::default(); 2];
        self.dirty = true;
    }

    fn set_slope(&mut self, value: f64) {
        if let Some(slope) = CutSlope::from_parameter(value)
            && self.slope != slope
        {
            self.slope = slope;
            self.reset();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SamplingMode {
    ProjectRate,
    Polyphase,
    Fir,
}

/// Dynamic bands plus low/high cuts with optional auto-gain.
pub struct DynamicEq {
    bands: [DynamicBand; BAND_COUNT],
    low_cut: Cut,
    high_cut: Cut,
    sample_rate: f64,
    bypass: bool,
    output_gain_db: f64,
    output_gain_lin: f64,
    auto_gain: bool,
    auto_gain_lin: f64,
    auto_gain_db: f64,
    auto_input_power: f64,
    auto_output_power: f64,
    auto_input_filters: [[KWeightChannel; 2]; 2],
    auto_output_filters: [[KWeightChannel; 2]; 2],
    auto_power_coeff: f64,
    auto_smooth_coeff: f64,
    auto_dirty: bool,
    oversample: bool,
    oversample_fir: bool,
    polyphase: Polyphase2x,
    fir: Fir2x,
}

impl DynamicEq {
    /// Prepare the parametric bands and the low/high cuts at the sample rate.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            bands: std::array::from_fn(|index| DynamicBand::new(index, sample_rate)),
            low_cut: Cut::new(FilterType::HighPass, MIN_FREQ, sample_rate),
            high_cut: Cut::new(FilterType::LowPass, MAX_FREQ, sample_rate),
            sample_rate: sample_rate as f64,
            bypass: false,
            output_gain_db: 0.0,
            output_gain_lin: 1.0,
            auto_gain: false,
            auto_gain_lin: 1.0,
            auto_gain_db: 0.0,
            auto_input_power: 0.0,
            auto_output_power: 0.0,
            auto_input_filters: [[KWeightChannel::default(); 2]; 2],
            auto_output_filters: [[KWeightChannel::default(); 2]; 2],
            auto_power_coeff: 0.0,
            auto_smooth_coeff: 0.0,
            auto_dirty: true,
            oversample: false,
            oversample_fir: false,
            polyphase: Polyphase2x::new(),
            fir: Fir2x::new(),
        }
    }

    fn reset_auto_gain(&mut self) {
        self.auto_gain_lin = 1.0;
        self.auto_gain_db = 0.0;
        self.auto_input_power = 0.0;
        self.auto_output_power = 0.0;
        for channel in &mut self.auto_input_filters {
            for filter in channel {
                filter.reset();
            }
        }
        for channel in &mut self.auto_output_filters {
            for filter in channel {
                filter.reset();
            }
        }
    }

    fn prepare_auto_gain(&mut self, sample_rate: f64) {
        if !self.auto_dirty {
            return;
        }
        if self.auto_gain {
            for channel in &mut self.auto_input_filters {
                *channel = [
                    KWeightChannel::new(sample_rate),
                    KWeightChannel::new(sample_rate),
                ];
            }
            for channel in &mut self.auto_output_filters {
                *channel = [
                    KWeightChannel::new(sample_rate),
                    KWeightChannel::new(sample_rate),
                ];
            }
        }
        self.auto_power_coeff = 1.0 - (-1.0 / (AUTO_GAIN_WINDOW_MS * 0.001 * sample_rate)).exp();
        self.auto_smooth_coeff = 1.0 - (-1.0 / (AUTO_GAIN_SMOOTH_MS * 0.001 * sample_rate)).exp();
        self.reset_auto_gain();
        self.auto_dirty = false;
    }

    fn prepare(&mut self, sample_rate: f64) {
        if (self.sample_rate - sample_rate).abs() > 0.5 {
            self.sample_rate = sample_rate;
            self.polyphase.reset();
            self.fir.reset();
            self.low_cut.reset();
            self.high_cut.reset();
            self.auto_dirty = true;
        }
        self.low_cut.prepare(sample_rate);
        self.high_cut.prepare(sample_rate);
        for band in &mut self.bands {
            band.prepare(sample_rate);
        }
        self.prepare_auto_gain(sample_rate);
    }

    /// Current detector level for band `index` (0-based), linear amplitude.
    pub fn band_envelope(&self, index: usize) -> Option<f64> {
        self.bands.get(index).map(|band| band.envelope)
    }

    /// Current dynamic gain offset for band `index`, in dB.
    pub fn band_dynamic_gain_db(&self, index: usize) -> Option<f64> {
        self.bands.get(index).map(|band| band.offset_db)
    }

    /// Configured static gain for band `index`, in dB.
    pub fn band_gain_db(&self, index: usize) -> Option<f64> {
        self.bands.get(index).map(|band| band.gain_db)
    }

    /// Current auto-gain makeup in dB (0 when disabled).
    pub fn auto_gain_db(&self) -> f64 {
        self.auto_gain_db
    }

    /// True when band `index` takes its detector from the external key input.
    pub fn band_uses_sidechain(&self, index: usize) -> Option<bool> {
        self.bands
            .get(index)
            .map(|band| band.dynamic && band.sidechain)
    }

    fn sampling_mode(&self) -> SamplingMode {
        match (self.oversample_fir, self.oversample) {
            (true, _) => SamplingMode::Fir,
            (false, true) => SamplingMode::Polyphase,
            (false, false) => SamplingMode::ProjectRate,
        }
    }

    fn process_inner(&mut self, buffer: &mut [f32], sample_rate: u32, sidechain: Option<&[f32]>) {
        if self.bypass {
            return;
        }
        let rate = f64::from(sample_rate);
        let mode = self.sampling_mode();
        let rate = if mode == SamplingMode::ProjectRate {
            rate
        } else {
            2.0 * rate
        };
        self.prepare(rate);
        let solo_active = self.bands.iter().any(|band| band.enabled && band.solo);
        let mut active = [0; BAND_COUNT];
        let mut count = 0;
        for (index, band) in self.bands.iter().enumerate() {
            if band.enabled && (!solo_active || band.solo) {
                active[count] = index;
                count += 1;
            }
        }
        let active = &active[..count];
        let key_at = |frame_index: usize| -> Option<[f64; 2]> {
            let index = frame_index * 2;
            let key = sidechain?.get(index..index + 2)?;
            Some([f64::from(key[0]), f64::from(key[1])])
        };
        let frames = buffer.as_chunks_mut::<2>().0;
        if mode != SamplingMode::ProjectRate {
            let use_key = self
                .bands
                .iter()
                .any(|band| band.enabled && band.dynamic && band.sidechain);
            for (index, frame) in frames.iter_mut().enumerate() {
                let input = frame.map(f64::from);
                let key = if use_key { key_at(index) } else { None };
                let (input, key) = if mode == SamplingMode::Fir {
                    (Fir2x::interpolate(input), key.map(Fir2x::interpolate))
                } else {
                    (
                        self.polyphase.interpolate(input),
                        key.map(|key| self.polyphase.interpolate_key(key)),
                    )
                };
                let filtered = std::array::from_fn(|phase| {
                    self.process_frame(input[phase], key.map(|pair| pair[phase]), active)
                });
                let output = if mode == SamplingMode::Fir {
                    self.fir.decimate(filtered)
                } else {
                    self.polyphase.decimate(filtered)
                };
                *frame = output.map(|sample| sample as f32);
            }
        } else {
            for (index, frame) in frames.iter_mut().enumerate() {
                *frame = self
                    .process_frame(frame.map(f64::from), key_at(index), active)
                    .map(|sample| sample as f32);
            }
        }
    }

    fn process_frame(
        &mut self,
        input: [f64; 2],
        key: Option<[f64; 2]>,
        active: &[usize],
    ) -> [f64; 2] {
        let [input_left, input_right] = input;
        let (mut left, mut right) = (input_left, input_right);
        if self.low_cut.enabled {
            (left, right) = self.low_cut.process_pair(left, right);
        }
        let key = key.map(|[left, right]| (left, right));
        for &index in active {
            (left, right) = self.bands[index].process_frame(left, right, key);
        }
        if self.high_cut.enabled {
            (left, right) = self.high_cut.process_pair(left, right);
        }
        if self.auto_gain {
            (left, right) = self.apply_auto_gain(input_left, input_right, left, right);
        }
        [left * self.output_gain_lin, right * self.output_gain_lin]
    }

    fn apply_auto_gain(
        &mut self,
        input_left: f64,
        input_right: f64,
        left: f64,
        right: f64,
    ) -> (f64, f64) {
        let input_l = self.auto_input_filters[0][0].process(input_left);
        let input_r = self.auto_input_filters[1][0].process(input_right);
        let input = [
            self.auto_input_filters[0][1].process(input_l),
            self.auto_input_filters[1][1].process(input_r),
        ];
        let output_l = self.auto_output_filters[0][0].process(left);
        let output_r = self.auto_output_filters[1][0].process(right);
        let output = [
            self.auto_output_filters[0][1].process(output_l),
            self.auto_output_filters[1][1].process(output_r),
        ];
        let power_coeff = self.auto_power_coeff;
        let input_power = (input[0] * input[0] + input[1] * input[1]) * 0.5;
        let output_power = (output[0] * output[0] + output[1] * output[1]) * 0.5;
        self.auto_input_power += power_coeff * (input_power - self.auto_input_power);
        self.auto_output_power += power_coeff * (output_power - self.auto_output_power);
        let ratio = if self.auto_output_power > MIN_AUTO_POWER {
            (self.auto_input_power / self.auto_output_power).sqrt()
        } else {
            1.0
        };
        let target = ratio.clamp(
            db_to_linear(-AUTO_GAIN_MAX_DB),
            db_to_linear(AUTO_GAIN_MAX_DB),
        );
        self.auto_gain_lin += self.auto_smooth_coeff * (target - self.auto_gain_lin);
        self.auto_gain_db = 20.0 * self.auto_gain_lin.max(1e-12).log10();
        (left * self.auto_gain_lin, right * self.auto_gain_lin)
    }
}

impl AudioEffect for DynamicEq {
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        self.process_inner(buffer, sample_rate, None);
    }

    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        sample_rate: u32,
        _transport: Option<oc_audio::PluginTransport>,
    ) {
        self.process_inner(buffer, sample_rate, Some(sidechain));
    }

    fn supports_sidechain(&self) -> bool {
        true
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "bypass" => self.bypass = value >= 0.5,
            "oversample" | "oversample_fir" => {
                let before = self.sampling_mode();
                if name == "oversample_fir" {
                    self.oversample_fir = value >= 0.5;
                } else {
                    self.oversample = value >= 0.5;
                }
                if self.sampling_mode() != before {
                    self.reset();
                }
            }
            "output_gain" => {
                let (min, max) = DYNAMIC_EQ_OUTPUT_GAIN_DB_RANGE;
                let gain = value.clamp(min, max);
                if gain != self.output_gain_db {
                    self.output_gain_db = gain;
                    self.output_gain_lin = db_to_linear(gain);
                }
            }
            "auto_gain" => {
                let auto_gain = value >= 0.5;
                if auto_gain != self.auto_gain {
                    self.auto_gain = auto_gain;
                    self.auto_dirty = true;
                }
            }
            "low_cut_enabled" => {
                let enabled = value >= 0.5;
                if enabled != self.low_cut.enabled {
                    self.low_cut.enabled = enabled;
                    self.low_cut.reset();
                }
            }
            "low_cut_freq" => {
                let freq = value.clamp(MIN_FREQ, MAX_FREQ);
                if freq != self.low_cut.freq {
                    self.low_cut.freq = freq;
                    self.low_cut.dirty = true;
                }
            }
            "low_cut_q" => {
                let q = value.clamp(MIN_Q, MAX_Q);
                if q != self.low_cut.q {
                    self.low_cut.q = q;
                    self.low_cut.dirty = true;
                }
            }
            "low_cut_slope" => self.low_cut.set_slope(value),
            "high_cut_enabled" => {
                let enabled = value >= 0.5;
                if enabled != self.high_cut.enabled {
                    self.high_cut.enabled = enabled;
                    self.high_cut.reset();
                }
            }
            "high_cut_freq" => {
                let freq = value.clamp(MIN_FREQ, MAX_FREQ);
                if freq != self.high_cut.freq {
                    self.high_cut.freq = freq;
                    self.high_cut.dirty = true;
                }
            }
            "high_cut_q" => {
                let q = value.clamp(MIN_Q, MAX_Q);
                if q != self.high_cut.q {
                    self.high_cut.q = q;
                    self.high_cut.dirty = true;
                }
            }
            "high_cut_slope" => self.high_cut.set_slope(value),
            _ => {
                let Some((index, field)) = band_param(name) else {
                    return;
                };
                if self.bands[index].set_field(field, value) {
                    self.bands[index].dirty = true;
                }
            }
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "bypass" => Some(if self.bypass { 1.0 } else { 0.0 }),
            "auto_gain" => Some(if self.auto_gain { 1.0 } else { 0.0 }),
            "oversample" => Some(f64::from(self.oversample)),
            "oversample_fir" => Some(f64::from(self.oversample_fir)),
            "output_gain" => Some(self.output_gain_db),
            "low_cut_enabled" => Some(if self.low_cut.enabled { 1.0 } else { 0.0 }),
            "low_cut_freq" => Some(self.low_cut.freq),
            "low_cut_q" => Some(self.low_cut.q),
            "low_cut_slope" => Some(self.low_cut.slope.parameter()),
            "high_cut_enabled" => Some(if self.high_cut.enabled { 1.0 } else { 0.0 }),
            "high_cut_freq" => Some(self.high_cut.freq),
            "high_cut_q" => Some(self.high_cut.q),
            "high_cut_slope" => Some(self.high_cut.slope.parameter()),
            _ => {
                let (index, field) = band_param(name)?;
                self.bands[index].get_field(field)
            }
        }
    }

    fn reset(&mut self) {
        self.low_cut.reset();
        self.high_cut.reset();
        for band in &mut self.bands {
            band.reset_runtime();
            band.dirty = true;
        }
        self.polyphase.reset();
        self.fir.reset();
        self.reset_auto_gain();
    }

    fn latency_samples(&self) -> u32 {
        if self.sampling_mode() == SamplingMode::Fir {
            FIR_2X_LATENCY as u32
        } else {
            0
        }
    }

    fn tail_samples(&self) -> u32 {
        2 * self.latency_samples()
    }

    /// Positive dB of dynamic attenuation; boosts report zero.
    fn gain_reduction_db(&self) -> f64 {
        let offset: f64 = self
            .bands
            .iter()
            .filter(|band| band.enabled && band.dynamic)
            .map(|band| band.offset_db)
            .sum();
        -offset.min(0.0)
    }

    fn band_count(&self) -> usize {
        BAND_COUNT
    }

    fn band_gain_db(&self, index: usize) -> Option<f64> {
        self.band_dynamic_gain_db(index)
    }

    fn name(&self) -> &str {
        "Dynamic EQ"
    }
}

fn band_param(name: &str) -> Option<(usize, &str)> {
    let rest = name.strip_prefix("band")?;
    let (index, field) = rest.split_once('_')?;
    let index: usize = index.parse().ok()?;
    if (1..=BAND_COUNT).contains(&index) {
        Some((index - 1, field))
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParamField {
    Switch,
    Type,
    Slope,
    Channel,
    Frequency,
    Q,
    Gain,
    Threshold,
    Range,
    Attack,
    Release,
}

pub(crate) fn param_field(name: &str) -> Option<ParamField> {
    if matches!(
        name,
        "bypass" | "auto_gain" | "oversample" | "oversample_fir"
    ) {
        return Some(ParamField::Switch);
    }
    if name == "output_gain" {
        return Some(ParamField::Gain);
    }
    if let Some(rest) = name
        .strip_prefix("low_cut_")
        .or_else(|| name.strip_prefix("high_cut_"))
    {
        return match rest {
            "enabled" => Some(ParamField::Switch),
            "freq" => Some(ParamField::Frequency),
            "q" => Some(ParamField::Q),
            "slope" => Some(ParamField::Slope),
            _ => None,
        };
    }
    let (_, field) = band_param(name)?;
    match field {
        "enabled" | "dynamic" | "solo" | "sidechain" => Some(ParamField::Switch),
        "type" => Some(ParamField::Type),
        "slope" => Some(ParamField::Slope),
        "channel" => Some(ParamField::Channel),
        "freq" => Some(ParamField::Frequency),
        "q" => Some(ParamField::Q),
        "gain" => Some(ParamField::Gain),
        "threshold" => Some(ParamField::Threshold),
        "range" => Some(ParamField::Range),
        "attack" => Some(ParamField::Attack),
        "release" => Some(ParamField::Release),
        _ => None,
    }
}

/// Whether the parameter selects the order of a low or high cut.
pub fn slope_param(name: &str) -> bool {
    matches!(param_field(name), Some(ParamField::Slope))
}

/// Whether a parameter id names a band frequency.
pub fn frequency_param(name: &str) -> bool {
    matches!(param_field(name), Some(ParamField::Frequency))
}

/// Whether a parameter id names a dB value.
pub fn db_param(name: &str) -> bool {
    matches!(
        param_field(name),
        Some(ParamField::Gain | ParamField::Threshold | ParamField::Range)
    )
}

/// Whether a parameter id names a band time in milliseconds.
pub fn time_ms_param(name: &str) -> bool {
    matches!(
        param_field(name),
        Some(ParamField::Attack | ParamField::Release)
    )
}

/// Whether a parameter id names a band switch.
pub fn switch_param(name: &str) -> bool {
    matches!(param_field(name), Some(ParamField::Switch))
}

/// Whether a parameter id names a band type.
pub fn type_param(name: &str) -> bool {
    matches!(param_field(name), Some(ParamField::Type))
}

/// Whether a parameter id names a detector channel target.
pub fn channel_param(name: &str) -> bool {
    matches!(param_field(name), Some(ParamField::Channel))
}

fn time_coefficient(milliseconds: f64, sample_rate: f64) -> f64 {
    (-1.0 / (milliseconds * 0.001 * sample_rate)).exp()
}

fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SR: u32 = 48_000;

    fn sine(freq: f64, amplitude: f64, samples: usize) -> Vec<f32> {
        let mut buffer = vec![0.0f32; samples * 2];
        for frame in 0..samples {
            let value = (TAU * freq * frame as f64 / SR as f64).sin() as f32 * amplitude as f32;
            buffer[frame * 2] = value;
            buffer[frame * 2 + 1] = value;
        }
        buffer
    }

    fn measured_gain_db(eq: &mut DynamicEq, freq: f64, amplitude: f64) -> f64 {
        measured_gain_db_channel(eq, freq, amplitude, 0)
    }

    fn measured_gain_db_channel(
        eq: &mut DynamicEq,
        freq: f64,
        amplitude: f64,
        channel: usize,
    ) -> f64 {
        let frames = SR as usize;
        let mut buffer = sine(freq, amplitude, frames);
        eq.reset();
        eq.process(&mut buffer, SR);
        let start = frames / 2;
        let energy: f64 = buffer[start * 2 + channel..]
            .iter()
            .step_by(2)
            .map(|sample| (*sample as f64).powi(2))
            .sum();
        let rms = (energy / (frames - start) as f64).sqrt();
        let input_rms = amplitude / 2f64.sqrt();
        20.0 * (rms / input_rms).log10()
    }

    /// Cookbook reference kept independent from the production coefficient helper.
    fn reference_gain_db(kind: BandKind, freq: f64, q: f64, gain_db: f64, at: f64) -> f64 {
        let w0 = TAU * freq / SR as f64;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q);
        let a = 10f64.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BandKind::Bell => (
                1.0 + alpha * a,
                -2.0 * cos_w0,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos_w0,
                1.0 - alpha / a,
            ),
            BandKind::LowShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
                    a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                    (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
                    (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
                )
            }
            BandKind::HighShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
                    a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                    (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
                    (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
                )
            }
            BandKind::LowCut => (
                (1.0 + cos_w0) * 0.5,
                -(1.0 + cos_w0),
                (1.0 + cos_w0) * 0.5,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
            BandKind::HighCut => (
                (1.0 - cos_w0) * 0.5,
                1.0 - cos_w0,
                (1.0 - cos_w0) * 0.5,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
            BandKind::Notch => (
                1.0,
                -2.0 * cos_w0,
                1.0,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
            BandKind::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos_w0, 1.0 - alpha),
        };
        let (b0, b1, b2, a1, a2) = (b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0);
        let w = TAU * at / SR as f64;
        let (sin_w, cos_w) = w.sin_cos();
        let (z1r, z1i) = (cos_w, -sin_w);
        let (z2r, z2i) = (cos_w * cos_w - sin_w * sin_w, -2.0 * sin_w * cos_w);
        let num_r = b0 + b1 * z1r + b2 * z2r;
        let num_i = b1 * z1i + b2 * z2i;
        let den_r = 1.0 + a1 * z1r + a2 * z2r;
        let den_i = a1 * z1i + a2 * z2i;
        20.0 * ((num_r * num_r + num_i * num_i).sqrt() / (den_r * den_r + den_i * den_i).sqrt())
            .log10()
    }

    #[test]
    fn static_response_matches_analytic_biquad_for_every_band_type() {
        let cases = [
            (
                BandKind::Bell,
                1_000.0,
                1.0,
                9.0,
                [100.0, 500.0, 1_000.0, 2_000.0, 8_000.0],
            ),
            (
                BandKind::LowShelf,
                500.0,
                0.707,
                9.0,
                [80.0, 250.0, 500.0, 2_000.0, 8_000.0],
            ),
            (
                BandKind::HighShelf,
                4_000.0,
                0.707,
                -9.0,
                [200.0, 1_000.0, 4_000.0, 8_000.0, 16_000.0],
            ),
            (
                BandKind::LowCut,
                200.0,
                0.707,
                0.0,
                [40.0, 100.0, 200.0, 1_000.0, 8_000.0],
            ),
            (
                BandKind::HighCut,
                6_000.0,
                0.707,
                0.0,
                [200.0, 2_000.0, 6_000.0, 12_000.0, 18_000.0],
            ),
            (
                BandKind::Notch,
                1_000.0,
                3.0,
                0.0,
                [200.0, 500.0, 900.0, 1_100.0, 4_000.0],
            ),
            (
                BandKind::BandPass,
                1_000.0,
                2.0,
                0.0,
                [100.0, 500.0, 1_000.0, 2_000.0, 8_000.0],
            ),
        ];
        for (kind, freq, q, gain, probe) in cases {
            let mut eq = DynamicEq::new(SR);
            eq.set_param("band1_enabled", 1.0);
            eq.set_param("band1_type", kind.code());
            eq.set_param("band1_freq", freq);
            eq.set_param("band1_q", q);
            eq.set_param("band1_gain", gain);
            for at in probe {
                let measured = measured_gain_db(&mut eq, at, 0.25);
                let expected = reference_gain_db(kind, freq, q, gain, at);
                assert!(
                    (measured - expected).abs() < 0.2,
                    "{kind:?} at {at} Hz: measured {measured:.3} dB, expected {expected:.3} dB"
                );
            }
        }
    }

    fn two_band_eq(channel: ChannelTarget) -> DynamicEq {
        let mut eq = DynamicEq::new(SR);
        for (index, freq) in [(1, 1_000.0), (2, 4_000.0)] {
            let prefix = format!("band{index}");
            eq.set_param(&format!("{prefix}_enabled"), 1.0);
            eq.set_param(&format!("{prefix}_type"), BandKind::Bell.code());
            eq.set_param(&format!("{prefix}_freq"), freq);
            eq.set_param(&format!("{prefix}_q"), 1.0);
            eq.set_param(&format!("{prefix}_gain"), 12.0);
            eq.set_param(&format!("{prefix}_channel"), channel.code());
        }
        eq
    }

    #[test]
    fn output_gain_scales_both_channels_after_auto_gain_and_survives_reset() {
        let mut reference = two_band_eq(ChannelTarget::Left);
        let mut trimmed = two_band_eq(ChannelTarget::Left);
        reference.set_param("auto_gain", 1.0);
        trimmed.set_param("auto_gain", 1.0);
        trimmed.set_param("output_gain", -6.0);
        trimmed.reset();
        let signal: Vec<f32> = (0..2048).map(|i| (i as f32 * 0.17).sin() * 0.2).collect();
        let mut expected = signal.clone();
        let mut actual = signal.clone();
        reference.process(&mut expected, SR);
        trimmed.process(&mut actual, SR);
        assert_eq!(trimmed.get_param("output_gain"), Some(-6.0));
        assert_eq!(trimmed.auto_gain_db(), reference.auto_gain_db());
        let scale = db_to_linear(-6.0) as f32;
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected * scale).abs() < 1e-6);
        }
        trimmed.set_param("bypass", 1.0);
        let mut bypassed = signal.clone();
        trimmed.process(&mut bypassed, SR);
        assert_eq!(bypassed, signal);
    }

    #[test]
    fn last_band_can_turn_on_without_changing_the_earlier_sections() {
        let mut eq = DynamicEq::new(SR);
        for index in 1..=BAND_COUNT {
            eq.set_param(&format!("band{index}_freq"), 2_000.0);
            eq.set_param(&format!("band{index}_gain"), 1.0);
            if index < BAND_COUNT {
                eq.set_param(&format!("band{index}_enabled"), 1.0);
            }
        }
        let last = format!("band{BAND_COUNT}_enabled");
        let baseline = (BAND_COUNT - 1) as f64;
        assert_eq!(eq.get_param(&last), Some(0.0));
        assert!((measured_gain_db(&mut eq, 2_000.0, 0.1) - baseline).abs() < 0.02);
        eq.set_param(&last, 1.0);
        assert!((measured_gain_db(&mut eq, 2_000.0, 0.1) - (baseline + 1.0)).abs() < 0.02);
        eq.set_param(
            &format!("band{BAND_COUNT}_channel"),
            ChannelTarget::Right.code(),
        );
        assert!((measured_gain_db(&mut eq, 2_000.0, 0.1) - baseline).abs() < 0.02);
        assert!(
            (measured_gain_db_channel(&mut eq, 2_000.0, 0.1, 1) - (baseline + 1.0)).abs() < 0.02
        );
    }

    #[test]
    fn cut_slope_matches_butterworth_at_the_corner_and_in_the_stopband() {
        use std::f64::consts::{FRAC_1_SQRT_2, PI};

        let corner = 1000.0;
        for (slot, kind, stop) in [
            ("low_cut", BandKind::LowCut, 500.0),
            ("high_cut", BandKind::HighCut, 2000.0),
        ] {
            for sections in 1..=oc_device_schema::DYNAMIC_EQ_CUT_SLOPES.len() {
                let slope = (sections - 1) as f64;
                for prefix in [slot, "band7"] {
                    let mut eq = DynamicEq::new(SR);
                    eq.set_param(&format!("{prefix}_enabled"), 1.0);
                    eq.set_param(&format!("{prefix}_freq"), corner);
                    eq.set_param(&format!("{prefix}_q"), FRAC_1_SQRT_2);
                    eq.set_param(&format!("{prefix}_slope"), slope);
                    if prefix == "band7" {
                        eq.set_param("band7_type", kind.code());
                    }
                    for freq in [corner, stop] {
                        let mut ratio =
                            (PI * freq / f64::from(SR)).tan() / (PI * corner / f64::from(SR)).tan();
                        if kind == BandKind::LowCut {
                            ratio = ratio.recip();
                        }
                        let expected = -10.0 * (1.0 + ratio.powi(4 * sections as i32)).log10();
                        let measured = measured_gain_db(&mut eq, freq, 0.2);
                        assert!(
                            (measured - expected).abs() < 0.02,
                            "{prefix} {sections} at {freq} Hz: {measured:.3} != {expected:.3}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_stored_cut_slope_does_not_change_a_bell() {
        let mut eq = two_band_eq(ChannelTarget::Stereo);
        let original = measured_gain_db(&mut eq, 1000.0, 0.1);
        eq.set_param("band1_slope", 3.0);
        assert_eq!(eq.get_param("band1_slope"), Some(3.0));
        assert_eq!(measured_gain_db(&mut eq, 1000.0, 0.1), original);
    }

    #[test]
    fn solo_restricts_processing_to_soloed_bands() {
        let mut eq = two_band_eq(ChannelTarget::Stereo);
        let band1_at_4k = reference_gain_db(BandKind::Bell, 1_000.0, 1.0, 12.0, 4_000.0);
        eq.set_param("band1_solo", 1.0);
        let soloed = measured_gain_db(&mut eq, 4_000.0, 0.1);
        assert!((soloed - band1_at_4k).abs() < 0.1, "soloed {soloed}");

        eq.set_param("band1_solo", 0.0);
        let both = measured_gain_db(&mut eq, 4_000.0, 0.1);
        assert!(
            both - soloed > 8.0,
            "band 2 did not return: {both} vs {soloed}"
        );
    }

    #[test]
    fn channel_targets_scope_a_band_to_one_component() {
        let mut mid = two_band_eq(ChannelTarget::Mid);
        mid.set_param("band2_enabled", 0.0);
        let (left, right) = measured_channels(&mut mid, 1_000.0, 0.1, false);
        assert!((left - 12.0).abs() < 0.2 && (right - 12.0).abs() < 0.2);
        let (left, _) = measured_channels(&mut mid, 1_000.0, 0.1, true);
        assert!(left.abs() < 0.1, "mid band leaked into side: {left} dB");

        let mut side = two_band_eq(ChannelTarget::Side);
        side.set_param("band2_enabled", 0.0);
        let (left, right) = measured_channels(&mut side, 1_000.0, 0.1, true);
        assert!((left - 12.0).abs() < 0.2 && (right - 12.0).abs() < 0.2);
        let (left, _) = measured_channels(&mut side, 1_000.0, 0.1, false);
        assert!(left.abs() < 0.1, "side band leaked into mid: {left} dB");

        let mut left_band = two_band_eq(ChannelTarget::Left);
        left_band.set_param("band2_enabled", 0.0);
        let (left, right) = measured_channels(&mut left_band, 1_000.0, 0.1, false);
        assert!((left - 12.0).abs() < 0.2);
        assert!(right.abs() < 0.1, "left band leaked into right: {right} dB");
    }

    fn measured_channels(
        eq: &mut DynamicEq,
        freq: f64,
        amplitude: f64,
        invert_right: bool,
    ) -> (f64, f64) {
        let frames = SR as usize;
        let mut buffer = vec![0f32; frames * 2];
        for frame in 0..frames {
            let value = (TAU * freq * frame as f64 / SR as f64).sin() * amplitude;
            buffer[frame * 2] = value as f32;
            buffer[frame * 2 + 1] = if invert_right {
                -value as f32
            } else {
                value as f32
            };
        }
        eq.reset();
        eq.process(&mut buffer, SR);
        let start = frames / 2;
        let input_rms = amplitude / 2f64.sqrt();
        let gain = |channel: usize| {
            let energy: f64 = buffer[start * 2 + channel..]
                .iter()
                .step_by(2)
                .map(|sample| (*sample as f64).powi(2))
                .sum();
            let rms = (energy / (frames - start) as f64).sqrt();
            20.0 * (rms / input_rms).log10()
        };
        (gain(0), gain(1))
    }

    fn dynamic_band(threshold: f64, range: f64, attack: f64, release: f64) -> DynamicEq {
        let mut eq = DynamicEq::new(SR);
        eq.set_param("band1_enabled", 1.0);
        eq.set_param("band1_type", BandKind::Bell.code());
        eq.set_param("band1_freq", 1_000.0);
        eq.set_param("band1_q", 1.0);
        eq.set_param("band1_gain", 0.0);
        eq.set_param("band1_dynamic", 1.0);
        eq.set_param("band1_threshold", threshold);
        eq.set_param("band1_range", range);
        eq.set_param("band1_attack", attack);
        eq.set_param("band1_release", release);
        eq
    }

    #[test]
    fn dynamic_gain_reaches_range_without_exceeding_it() {
        let mut eq = dynamic_band(-40.0, -12.0, 5.0, 100.0);
        let mut buffer = sine(1_000.0, 1.0, SR as usize);
        let mut lowest = 0.0f64;
        let mut highest = 0.0f64;
        for chunk in buffer.chunks_mut(256) {
            eq.process(chunk, SR);
            let offset = eq.band_dynamic_gain_db(0).unwrap();
            lowest = lowest.min(offset);
            highest = highest.max(offset);
        }
        assert!(lowest >= -12.0 - 1e-9, "exceeded range: {lowest}");
        assert!(highest <= 1e-9, "unexpected boost: {highest}");
        assert!((lowest + 12.0).abs() < 0.1, "did not reach range: {lowest}");
    }

    #[test]
    fn refreshing_identical_parameters_keeps_detector_state() {
        let mut eq = dynamic_band(-40.0, -6.0, 5.0, 100.0);
        let mut settle = vec![1.0f32; 8_192];
        eq.process(&mut settle, SR);
        let before = eq.band_envelope(0).unwrap();
        for (name, value) in [
            ("band1_enabled", 1.0),
            ("band1_type", 0.0),
            ("band1_freq", 1_000.0),
            ("band1_q", 1.0),
            ("band1_gain", 0.0),
            ("band1_dynamic", 1.0),
            ("band1_threshold", -40.0),
            ("band1_range", -6.0),
            ("band1_attack", 5.0),
            ("band1_release", 100.0),
        ] {
            eq.set_param(name, value);
        }
        let mut more = vec![1.0f32; 128];
        eq.process(&mut more, SR);
        let after = eq.band_envelope(0).unwrap();
        assert!(
            after >= before * 0.99,
            "detector reset: {before} -> {after}"
        );
    }

    #[test]
    fn positive_range_expands_above_threshold() {
        let mut eq = dynamic_band(-60.0, 6.0, 2.0, 50.0);
        let mut buffer = sine(1_000.0, 0.01, SR as usize / 2);
        for chunk in buffer.chunks_mut(256) {
            eq.process(chunk, SR);
        }
        let offset = eq.band_dynamic_gain_db(0).unwrap();
        assert!(
            (offset - 6.0).abs() < 0.1,
            "expansion did not reach range: {offset}"
        );
    }

    #[test]
    fn dynamic_attack_and_release_track_their_time_constants() {
        // A DC step isolates the detector's time constants from waveform ripple.
        let mut eq = dynamic_band(-60.0, 0.0, 20.0, 200.0);
        let mut steady = vec![1.0f32; SR as usize * 2];
        eq.process(&mut steady, SR);
        let steady_level = eq.band_envelope(0).unwrap();
        assert!(
            (steady_level - 1.0).abs() < 0.01,
            "detector level {steady_level}"
        );

        eq.reset();
        let mut attack_buffer = vec![1.0f32; SR as usize * 2];
        let mut attack_ms = None;
        for (block, chunk) in attack_buffer.chunks_mut(64).enumerate() {
            eq.process(chunk, SR);
            if eq.band_envelope(0).unwrap() >= steady_level * (1.0 - (-1.0f64).exp()) {
                attack_ms = Some((block + 1) * 32 * 1_000 / SR as usize);
                break;
            }
        }
        let attack_ms = attack_ms.expect("attack never crossed 63%");
        assert!(
            (attack_ms as f64 - 20.0).abs() <= 4.0,
            "attack {attack_ms} ms"
        );

        eq.reset();
        let mut settle = vec![1.0f32; SR as usize * 2];
        eq.process(&mut settle, SR);
        let release_start = eq.band_envelope(0).unwrap();
        let mut release_buffer = vec![0.0f32; SR as usize * 2];
        let mut release_ms = None;
        for (block, chunk) in release_buffer.chunks_mut(64).enumerate() {
            eq.process(chunk, SR);
            if eq.band_envelope(0).unwrap() <= release_start * (-1.0f64).exp() {
                release_ms = Some((block + 1) * 32 * 1_000 / SR as usize);
                break;
            }
        }
        let release_ms = release_ms.expect("release never crossed 37%");
        assert!(
            (release_ms as f64 - 200.0).abs() <= 40.0,
            "release {release_ms} ms"
        );
    }

    #[test]
    fn external_sidechain_drives_flagged_bands_without_leaking_into_audio() {
        let mut eq = dynamic_band(-40.0, -12.0, 5.0, 100.0);
        eq.set_param("band1_sidechain", 1.0);
        eq.set_param("band2_enabled", 1.0);
        eq.set_param("band2_type", BandKind::Bell.code());
        eq.set_param("band2_freq", 2_000.0);
        eq.set_param("band2_q", 1.0);
        eq.set_param("band2_dynamic", 1.0);
        eq.set_param("band2_threshold", -40.0);
        eq.set_param("band2_range", -12.0);

        let key = sine(1_000.0, 1.0, SR as usize);
        let mut quiet = vec![0.0f32; SR as usize * 2];
        eq.process_with_sidechain(&mut quiet, &key, SR, None);
        assert!(
            quiet.iter().all(|sample| sample.abs() < 1e-12),
            "external key leaked into the audio path"
        );
        assert!(
            eq.band_dynamic_gain_db(0).unwrap() < -11.5,
            "sidechain did not drive band 1: {:?}",
            eq.band_dynamic_gain_db(0)
        );
        assert_eq!(eq.band_dynamic_gain_db(1).unwrap(), 0.0);

        eq.reset();
        let mut main = sine(1_000.0, 0.001, SR as usize);
        let unchanged = main.clone();
        eq.process_with_sidechain(&mut main, &vec![0.0; SR as usize * 2], SR, None);
        assert_eq!(main, unchanged, "silent sidechain moved the audio path");
        assert_eq!(eq.band_dynamic_gain_db(0).unwrap(), 0.0);
    }

    #[test]
    fn moving_a_cut_preserves_history_through_sample_rate_automation() {
        let mut eq = DynamicEq::new(SR);
        eq.set_param("band12_enabled", 1.0);
        eq.set_param("band12_type", BandKind::HighCut.code());
        let mut previous = [0.0; 2];
        for frame in 0..480 {
            let frequency = 600.0 + f64::from(frame) * 8.0;
            let (sin, cos) = (TAU * frequency / f64::from(SR)).sin_cos();
            let a0 = 1.0 + sin / 2.0;
            let b0 = (1.0 - cos) / (2.0 * a0);
            let a1 = -2.0 * cos / a0;
            let a2 = (1.0 - sin / 2.0) / a0;
            let input = if frame == 0 { 0.25 } else { 0.0 };
            let feed = match frame {
                0 | 2 => b0 * 0.25,
                1 => 2.0 * b0 * 0.25,
                _ => 0.0,
            };
            let expected = feed - a1 * previous[0] - a2 * previous[1];
            previous = [expected, previous[0]];

            let mut audio = [input as f32; 2];
            eq.set_param("band12_freq", frequency);
            eq.process(&mut audio, SR);
            assert!(
                (f64::from(audio[0]) - expected).abs() < 1e-8,
                "frame {frame}"
            );
            assert_eq!(audio[0], audio[1]);
        }
    }

    #[test]
    fn retuning_does_not_discharge_a_dynamic_bands_detector() {
        let mut eq = DynamicEq::new(SR);
        eq.set_param("band1_enabled", 1.0);
        eq.set_param("band1_dynamic", 1.0);
        eq.set_param("band1_sidechain", 1.0);
        let mut main = vec![0.0; 8_192];
        let key = sine(1_000.0, 0.5, main.len() / 2);
        eq.process_with_sidechain(&mut main, &key, SR, None);
        let before = eq.band_envelope(0).unwrap();
        assert!(before > 0.1);

        eq.set_param("band1_freq", 1500.0);
        eq.set_param("band1_q", 2.0);
        eq.set_param("band1_gain", -3.0);
        eq.process_with_sidechain(&mut [0.0; 2], &[0.0; 2], SR, None);
        assert!(eq.band_envelope(0).unwrap() > before * 0.99);
    }

    #[test]
    fn high_cut_preserves_the_authored_corner_at_44_1_khz() {
        let rate = 44_100;
        let mut eq = DynamicEq::new(rate);
        eq.set_param("high_cut_enabled", 1.0);
        eq.set_param("high_cut_freq", 20_000.0);
        eq.set_param("high_cut_q", std::f64::consts::FRAC_1_SQRT_2);

        let input: Vec<f32> = (0..rate / 5)
            .flat_map(|frame| {
                let phase = std::f64::consts::TAU * 20_000.0 * f64::from(frame) / f64::from(rate);
                [0.25 * phase.sin() as f32; 2]
            })
            .collect();
        let mut output = input.clone();
        eq.process(&mut output, rate);

        let power = |data: &[f32]| {
            data[data.len() / 2..]
                .iter()
                .map(|&sample| f64::from(sample).powi(2))
                .sum::<f64>()
        };
        assert!((power(&output) / power(&input) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn oversampled_eq_is_partition_invariant_and_resets_its_history() {
        let make_eq = || {
            let mut eq = DynamicEq::new(SR);
            eq.set_param("oversample", 1.0);
            eq.set_param("band1_enabled", 1.0);
            eq.set_param("band1_freq", 13_000.0);
            eq.set_param("band1_gain", -8.0);
            eq
        };
        let mut fixture = sine(7_000.0, 0.25, 2048);
        for frame in fixture.as_chunks_mut::<2>().0 {
            frame[1] = 0.0;
        }
        let mut expected = fixture.clone();
        let mut whole = make_eq();
        whole.process(&mut expected, SR);
        assert_ne!(expected, fixture);
        assert_eq!(whole.latency_samples(), 0);
        assert!(
            expected
                .iter()
                .skip(1)
                .step_by(2)
                .all(|sample| *sample == 0.0)
        );

        let mut split = fixture.clone();
        let mut partitioned = make_eq();
        for block in split.chunks_mut(2 * 137) {
            partitioned.process(block, SR);
        }
        assert_eq!(expected, split);
        partitioned.reset();
        partitioned.process(&mut fixture, SR);
        assert_eq!(expected, fixture);
    }

    #[test]
    fn oversampled_external_key_changes_the_detector_and_stays_out_of_the_output() {
        let mut eq = dynamic_band(-40.0, -12.0, 5.0, 100.0);
        eq.set_param("oversample", 1.0);
        eq.set_param("band1_sidechain", 1.0);
        let key = sine(1_000.0, 1.0, 8192);
        let mut silent = vec![0.0; key.len()];
        eq.process_with_sidechain(&mut silent, &key, SR, None);
        assert!(silent.iter().all(|sample| *sample == 0.0));
        assert!(eq.band_dynamic_gain_db(0).unwrap() < -11.5);

        eq.set_param("oversample", 0.0);
        assert_eq!(eq.band_dynamic_gain_db(0).unwrap(), 0.0);
        assert_eq!(eq.get_param("oversample"), Some(0.0));
    }

    #[test]
    fn auto_gain_restores_a_tonal_level_within_half_a_db() {
        let frames = SR as usize * 6;
        let input = sine(1_000.0, 0.25, frames);
        let mut with_auto = dynamic_band(-40.0, -12.0, 5.0, 100.0);
        with_auto.set_param("band1_gain", 0.0);
        with_auto.set_param("auto_gain", 1.0);
        let mut auto_output = input.clone();
        with_auto.process(&mut auto_output, SR);

        let mut without_auto = dynamic_band(-40.0, -12.0, 5.0, 100.0);
        without_auto.set_param("band1_gain", 0.0);
        let mut dry_output = input.clone();
        without_auto.process(&mut dry_output, SR);

        let rms = |buffer: &[f32]| {
            let start = SR as usize * 4;
            let energy: f64 = buffer[start * 2..]
                .iter()
                .map(|sample| (*sample as f64).powi(2))
                .sum();
            (energy / (buffer.len() - start * 2) as f64).sqrt()
        };
        let input_db = 20.0 * rms(&input).log10();
        let auto_db = 20.0 * rms(&auto_output).log10();
        let dry_db = 20.0 * rms(&dry_output).log10();
        assert!(
            (auto_db - input_db).abs() <= 0.5,
            "auto-gain level {auto_db:.3} dB vs input {input_db:.3} dB"
        );
        assert!(
            dry_db < input_db - 8.0,
            "the dynamic band did not move without auto-gain: {dry_db:.3} dB"
        );
        assert!(
            (with_auto.auto_gain_db() - 12.0).abs() < 0.5,
            "makeup {:?} dB",
            with_auto.auto_gain_db()
        );
    }

    #[test]
    fn bypass_and_disabled_bands_are_bit_transparent() {
        let mut reference = vec![0.0f32; SR as usize * 2];
        for (frame, pair) in reference.chunks_exact_mut(2).enumerate() {
            let value = (frame as f32 * 0.017).sin() * 0.4;
            pair[0] = value;
            pair[1] = -value;
        }

        let mut bypassed = reference.clone();
        let mut eq = DynamicEq::new(SR);
        eq.set_param("bypass", 1.0);
        eq.process(&mut bypassed, SR);
        assert_eq!(bypassed, reference);

        let mut disabled = reference.clone();
        eq.set_param("bypass", 0.0);
        eq.process(&mut disabled, SR);
        assert_eq!(disabled, reference);

        let mut flat = DynamicEq::new(SR);
        flat.set_param("band1_enabled", 1.0);
        flat.set_param("band1_freq", 1_000.0);
        flat.set_param("band1_q", 1.0);
        flat.set_param("band1_gain", 0.0);
        flat.set_param("band1_dynamic", 0.0);
        let mut static_band = reference.clone();
        flat.process(&mut static_band, SR);
        assert_eq!(static_band, reference);

        flat.set_param("band1_dynamic", 1.0);
        flat.set_param("band1_threshold", 0.0);
        flat.set_param("band1_range", -24.0);
        let mut below_threshold = reference.clone();
        flat.process(&mut below_threshold, SR);
        assert_eq!(below_threshold, reference);
    }

    #[test]
    #[ignore = "manual CPU sanity"]
    fn fully_active_dynamic_eq_cpu_sanity() {
        let mut eq = DynamicEq::new(SR);
        for band in 1..=BAND_COUNT {
            let prefix = format!("band{band}");
            eq.set_param(&format!("{prefix}_enabled"), 1.0);
            eq.set_param(&format!("{prefix}_type"), BandKind::Bell.code());
            eq.set_param(&format!("{prefix}_freq"), 80.0 * band as f64);
            eq.set_param(&format!("{prefix}_q"), 1.0);
            eq.set_param(&format!("{prefix}_gain"), 3.0);
            eq.set_param(&format!("{prefix}_dynamic"), 1.0);
            eq.set_param(&format!("{prefix}_threshold"), -30.0);
            eq.set_param(&format!("{prefix}_range"), -6.0);
            eq.set_param(&format!("{prefix}_attack"), 5.0);
            eq.set_param(&format!("{prefix}_release"), 100.0);
        }
        eq.set_param("low_cut_enabled", 1.0);
        eq.set_param("low_cut_freq", 30.0);
        eq.set_param("high_cut_enabled", 1.0);
        eq.set_param("high_cut_freq", 18_000.0);

        let mut buffer = sine(440.0, 0.5, 512);
        let frames = SR as usize * 10;
        let start = std::time::Instant::now();
        for _ in 0..(frames / 512) {
            eq.process(&mut buffer, SR);
        }
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "{BAND_COUNT}-band dynamic EQ: {:.3}x realtime, {:.2} ns/frame",
            frames as f64 / SR as f64 / elapsed,
            elapsed * 1e9 / frames as f64
        );

        let mut legacy = super::super::eq::ParametricEq::new(SR);
        legacy.set_param("freq", 1_000.0);
        legacy.set_param("gain", 3.0);
        let mut legacy_buffer = sine(440.0, 0.5, 512);
        let start = std::time::Instant::now();
        for _ in 0..(frames / 512) {
            legacy.process(&mut legacy_buffer, SR);
        }
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "legacy 3-band EQ: {:.3}x realtime, {:.2} ns/frame",
            frames as f64 / SR as f64 / elapsed,
            elapsed * 1e9 / frames as f64
        );
    }
}
