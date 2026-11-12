//! Vocal pitch corrector: YIN detection, a note state machine and PSOLA
//! resynthesis at the original rate.
//!
//! Stage 1 is the detector: a band-limited copy at ~16 kHz, YIN over a 40 ms
//! window, a median-filtered period track and octave-jump repair. All buffers
//! are allocated in the constructors; detection never allocates.

use std::f64::consts::TAU;

use super::AudioEffect;
use super::psola;

/// Decimated analysis rate the detector is designed around.
pub const ANALYSIS_RATE: f64 = 16_000.0;
/// YIN analysis window in the full-range mode.
pub const FULL_WINDOW_S: f64 = 0.040;
/// YIN analysis window in the low-latency mode.
pub const LOW_WINDOW_S: f64 = 0.024;
/// Lowest detected f0 in full-range mode.
pub const MIN_F0: f64 = 80.0;
/// Highest detected f0.
pub const MAX_F0: f64 = 1_000.0;
/// Low-latency mode trades the bottom of the range for a shorter window.
pub const LOW_MIN_F0: f64 = 120.0;
/// Absolute-threshold rule from the cumulative-mean normalized difference.
const YIN_THRESHOLD: f64 = 0.15;
/// Accept the global minimum without an absolute-threshold crossing only
/// below this value; anything higher is unvoiced.
const YIN_UNCERTAIN: f64 = 0.55;
const SILENCE_RMS: f64 = 1e-4;
const ANTI_ALIAS_TAPS: usize = 31;
const TRACK_HISTORY: usize = 5;

/// Integer decimator with a Blackman-windowed sinc low-pass.
pub struct Decimator {
    factor: usize,
    taps: Vec<f64>,
    ring: Vec<f64>,
    mask: usize,
    write: usize,
    phase: usize,
    rate: f64,
}

impl Decimator {
    /// Prepare an integer decimator for the analysis rate.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let factor = ((sample_rate as f64 / ANALYSIS_RATE).round() as usize).max(1);
        let rate = sample_rate as f64 / factor as f64;
        let mut taps = Vec::new();
        if factor > 1 {
            let cutoff = 0.45 * rate / sample_rate as f64;
            let center = (ANTI_ALIAS_TAPS - 1) as f64 * 0.5;
            let mut sum = 0.0;
            for index in 0..ANTI_ALIAS_TAPS {
                let offset = index as f64 - center;
                let sinc = if offset.abs() < 1e-9 {
                    2.0 * cutoff
                } else {
                    (TAU * cutoff * offset).sin() / (std::f64::consts::PI * offset)
                };
                let phase = TAU * index as f64 / (ANTI_ALIAS_TAPS - 1) as f64;
                let window = 0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos();
                taps.push(sinc * window);
                sum += sinc * window;
            }
            for tap in &mut taps {
                *tap /= sum;
            }
        }
        let ring_len = ANTI_ALIAS_TAPS.next_power_of_two();
        Self {
            factor,
            taps,
            ring: vec![0.0; ring_len],
            mask: ring_len - 1,
            write: 0,
            phase: 0,
            rate,
        }
    }

    /// Decimation factor.
    pub fn factor(&self) -> usize {
        self.factor
    }

    /// Decimated analysis rate in Hz.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Clear the filter history and phase.
    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.write = 0;
        self.phase = 0;
    }

    /// Push one full-rate sample; returns one decimated sample every `factor`.
    pub fn push(&mut self, sample: f64) -> Option<f64> {
        if self.factor == 1 {
            return Some(sample);
        }
        self.ring[self.write] = sample;
        self.write = (self.write + 1) & self.mask;
        if self.phase != 0 {
            self.phase += 1;
            if self.phase == self.factor {
                self.phase = 0;
            }
            return None;
        }
        self.phase = 1;
        let mut sum = 0.0;
        let mut index = self.write;
        for tap in &self.taps {
            index = (index + self.mask) & self.mask;
            sum += tap * self.ring[index];
        }
        Some(sum)
    }
}

/// YIN period estimate over one analysis frame.
pub struct YinDetector {
    window: usize,
    tau_min: usize,
    tau_max: usize,
    ring: Vec<f64>,
    mask: usize,
    write: usize,
    fill: usize,
    frame: Vec<f64>,
    difference: Vec<f64>,
    cumulative: Vec<f64>,
    pub period: f64,
    pub confidence: f64,
    pub level: f64,
}

impl YinDetector {
    /// Prepare YIN analysis for `rate`, window length and lowest f0.
    pub fn new(rate: f64, window_s: f64, min_f0: f64) -> Self {
        let window = ((rate * window_s).round() as usize).max(16);
        let tau_min = ((rate / MAX_F0).floor() as usize).max(2);
        let tau_max = ((rate / min_f0).ceil() as usize) + 1;
        let span = window + tau_max + 1;
        let ring_len = span.next_power_of_two();
        Self {
            window,
            tau_min,
            tau_max,
            ring: vec![0.0; ring_len],
            mask: ring_len - 1,
            write: 0,
            fill: 0,
            frame: vec![0.0; span],
            difference: vec![0.0; tau_max + 1],
            cumulative: vec![1.0; tau_max + 1],
            period: 0.0,
            confidence: 0.0,
            level: 0.0,
        }
    }

    /// Analysis window length in decimated samples.
    pub fn window(&self) -> usize {
        self.window
    }

    /// Largest lag searched, in decimated samples.
    pub fn tau_max(&self) -> usize {
        self.tau_max
    }

    /// Push one decimated sample into the analysis ring.
    pub fn push(&mut self, sample: f64) {
        self.ring[self.write] = sample;
        self.write = (self.write + 1) & self.mask;
        self.fill = (self.fill + 1).min(self.ring.len());
    }

    /// Analyse the most recent frame. Returns the raw period in decimated
    /// samples and its confidence, or `None` when the frame is unvoiced.
    pub fn detect(&mut self) -> Option<(f64, f64)> {
        let span = self.window + self.tau_max + 1;
        if self.fill < span {
            return None;
        }
        let start = (self.write + self.ring.len() - span) & self.mask;
        for (index, slot) in self.frame.iter_mut().enumerate() {
            *slot = self.ring[(start + index) & self.mask];
        }
        let mut energy = 0.0;
        for sample in &self.frame[..self.window] {
            energy += sample * sample;
        }
        self.level = (energy / self.window as f64).sqrt();
        if self.level < SILENCE_RMS {
            self.confidence = 0.0;
            return None;
        }

        for tau in 0..=self.tau_max {
            let mut sum = 0.0;
            for index in 0..self.window {
                let delta = self.frame[index] - self.frame[index + tau];
                sum += delta * delta;
            }
            self.difference[tau] = sum;
        }
        self.cumulative[0] = 1.0;
        let mut running = 0.0;
        for tau in 1..=self.tau_max {
            running += self.difference[tau];
            self.cumulative[tau] = if running > 0.0 {
                self.difference[tau] * tau as f64 / running
            } else {
                1.0
            };
        }

        let mut chosen = None;
        for tau in self.tau_min..=self.tau_max {
            if self.cumulative[tau] < YIN_THRESHOLD {
                let mut dip = tau;
                while dip < self.tau_max && self.cumulative[dip + 1] < self.cumulative[dip] {
                    dip += 1;
                }
                chosen = Some(dip);
                break;
            }
        }
        let dip = match chosen {
            Some(dip) => dip,
            None => {
                let mut best = self.tau_min;
                let mut value = f64::INFINITY;
                for tau in self.tau_min..=self.tau_max {
                    if self.cumulative[tau] < value {
                        value = self.cumulative[tau];
                        best = tau;
                    }
                }
                if value > YIN_UNCERTAIN {
                    self.confidence = 0.0;
                    return None;
                }
                best
            }
        };

        // The raw difference is locally quadratic at the dip; cmnd's 1/tau
        // weighting biases the interpolation at high f0.
        let period = parabolic_dip(&self.difference, dip, self.tau_min, self.tau_max);
        if !period.is_finite() || period <= 0.0 {
            self.confidence = 0.0;
            return None;
        }
        self.period = period;
        self.confidence = (1.0 - self.cumulative[dip]).clamp(0.0, 1.0);
        Some((period, self.confidence))
    }

    /// Clear the analysis ring and the last estimate.
    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.write = 0;
        self.fill = 0;
        self.period = 0.0;
        self.confidence = 0.0;
        self.level = 0.0;
    }
}

fn parabolic_dip(values: &[f64], dip: usize, low: usize, high: usize) -> f64 {
    if dip <= low || dip >= high {
        return dip as f64;
    }
    let left = values[dip - 1];
    let center = values[dip];
    let right = values[dip + 1];
    let denominator = left - 2.0 * center + right;
    if denominator.abs() < 1e-12 {
        return dip as f64;
    }
    let offset = (0.5 * (left - right) / denominator).clamp(-0.5, 0.5);
    dip as f64 + offset
}

/// Median-filtered period track with octave-jump repair.
pub struct PeriodTracker {
    history: [f64; TRACK_HISTORY],
    count: usize,
    next: usize,
    invalid_run: usize,
    pub period: f64,
    pub confidence: f64,
    pub valid: bool,
    pub f0: f64,
}

impl Default for PeriodTracker {
    fn default() -> Self {
        Self {
            history: [0.0; TRACK_HISTORY],
            count: 0,
            next: 0,
            invalid_run: 0,
            period: 0.0,
            confidence: 0.0,
            valid: false,
            f0: 0.0,
        }
    }
}

impl PeriodTracker {
    /// Fold one YIN estimate into the median period track.
    pub fn update(&mut self, estimate: Option<(f64, f64)>, rate: f64) {
        let Some((mut period, confidence)) = estimate else {
            self.invalid_run += 1;
            self.valid = false;
            self.confidence = 0.0;
            if self.invalid_run >= 3 {
                self.count = 0;
                self.next = 0;
                self.period = 0.0;
                self.f0 = 0.0;
            }
            return;
        };
        if self.count >= 3 && self.period > 0.0 {
            let ratio = period / self.period;
            if ratio > 1.75 {
                let half = period * 0.5;
                if (half / self.period - 1.0).abs() < 0.2 && half >= 2.0 {
                    period = half;
                }
            } else if ratio < 0.57 {
                let doubled = period * 2.0;
                if (doubled / self.period - 1.0).abs() < 0.2 {
                    period = doubled;
                }
            }
        }
        self.history[self.next] = period;
        self.next = (self.next + 1) % TRACK_HISTORY;
        self.count = (self.count + 1).min(TRACK_HISTORY);
        let mut sorted = self.history;
        let slice = &mut sorted[..self.count];
        slice.sort_by(|left, right| left.total_cmp(right));
        let median = slice[self.count / 2];
        self.period = median;
        self.f0 = rate / median;
        self.confidence = confidence;
        self.valid = true;
        self.invalid_run = 0;
    }

    /// Clear the period history and voicing state.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Semitone offsets from the scale root. Chromatic is the no-assumption set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    Chromatic,
    Major,
    NaturalMinor,
    HarmonicMinor,
    Dorian,
    Mixolydian,
    Lydian,
    Phrygian,
    Locrian,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
}

impl Scale {
    /// Display names indexed like `ALL`.
    pub const NAMES: [&'static str; 12] = oc_device_schema::SCALE_NAMES;

    /// Every selectable scale.
    pub const ALL: [Self; 12] = [
        Self::Chromatic,
        Self::Major,
        Self::NaturalMinor,
        Self::HarmonicMinor,
        Self::Dorian,
        Self::Mixolydian,
        Self::Lydian,
        Self::Phrygian,
        Self::Locrian,
        Self::MajorPentatonic,
        Self::MinorPentatonic,
        Self::Blues,
    ];

    /// Position of this scale in `ALL`.
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|scale| *scale == self)
            .unwrap_or(0)
    }

    /// The scale at `index`, or chromatic when out of range.
    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Self::Chromatic)
    }

    /// Parse a scale name or alias.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase().replace('_', " ");
        match name.as_str() {
            "chromatic" | "chroma" => Some(Self::Chromatic),
            "major" | "ionian" => Some(Self::Major),
            "minor" | "natural minor" | "aeolian" => Some(Self::NaturalMinor),
            "harmonic minor" | "harmonicminor" => Some(Self::HarmonicMinor),
            "dorian" => Some(Self::Dorian),
            "mixolydian" => Some(Self::Mixolydian),
            "lydian" => Some(Self::Lydian),
            "phrygian" => Some(Self::Phrygian),
            "locrian" => Some(Self::Locrian),
            "major pentatonic" | "pentatonic major" => Some(Self::MajorPentatonic),
            "minor pentatonic" | "pentatonic minor" => Some(Self::MinorPentatonic),
            "blues" => Some(Self::Blues),
            _ => None,
        }
    }

    /// Semitone offsets from the scale root.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            Self::Chromatic => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
            Self::Major => &[0, 2, 4, 5, 7, 9, 11],
            Self::NaturalMinor => &[0, 2, 3, 5, 7, 8, 10],
            Self::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            Self::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            Self::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            Self::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            Self::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            Self::Locrian => &[0, 1, 3, 5, 6, 8, 10],
            Self::MajorPentatonic => &[0, 2, 4, 7, 9],
            Self::MinorPentatonic => &[0, 3, 5, 7, 10],
            Self::Blues => &[0, 3, 5, 6, 7, 10],
        }
    }
}

/// The scale set and root are chosen explicitly by the engineer; the device
/// does not infer key from the audio.
pub fn scale_name(name: &str) -> Option<f64> {
    Scale::from_name(name).map(|scale| scale.index() as f64)
}

/// Note-level scale snap: the nearest allowed note to a continuous MIDI pitch.
pub fn snap_to_scale(midi: f64, key: u8, scale: Scale) -> i32 {
    if !midi.is_finite() {
        return 69;
    }
    let intervals = scale.intervals();
    let root = i32::from(key % 12);
    let lower = midi.floor() as i32 - 2;
    let upper = midi.ceil() as i32 + 2;
    let mut best = lower;
    let mut best_distance = f64::INFINITY;
    for note in lower..=upper {
        let class = (note - root).rem_euclid(12) as u8;
        if !intervals.contains(&class) {
            continue;
        }
        let distance = (note as f64 - midi).abs();
        if distance < best_distance - 1e-9 {
            best_distance = distance;
            best = note;
        }
    }
    best
}

/// Note phase; `Release` keeps the last target while the wet gain falls away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Lifecycle stage of a tracked note.
pub enum NotePhase {
    Idle,
    Onset,
    Steady,
    Release,
}

/// Note boundaries and the note-level target used by the correction.
///
/// Onset needs two valid frames; a note switches only after the tracked pitch
/// leaves a 60 cent hysteresis band, and a legato jump of 150 cents starts a
/// new note after three frames. That band is wider than the intended vibrato,
/// so 6 Hz vibrato cannot re-trigger a note.
pub struct NoteTracker {
    key: u8,
    scale: Scale,
    phase: NotePhase,
    valid_run: usize,
    invalid_run: usize,
    phase_frames: usize,
    jump_frames: usize,
    target: i32,
    released_target: i32,
}

const ONSET_VALID_FRAMES: usize = 2;
const ONSET_HOLD_FRAMES: usize = 6;
const RELEASE_INVALID_FRAMES: usize = 2;
const RELEASE_HOLD_FRAMES: usize = 10;
const SWITCH_CENTS: f64 = 60.0;
const RETRIGGER_CENTS: f64 = 150.0;
const RETRIGGER_FRAMES: usize = 3;

impl NoteTracker {
    /// Track notes in `key` and `scale`.
    pub fn new(key: u8, scale: Scale) -> Self {
        Self {
            key: key % 12,
            scale,
            phase: NotePhase::Idle,
            valid_run: 0,
            invalid_run: 0,
            phase_frames: 0,
            jump_frames: 0,
            target: 69,
            released_target: 69,
        }
    }

    /// Change the key and scale; the next update re-targets the note.
    pub fn set_scale(&mut self, key: u8, scale: Scale) {
        self.key = key % 12;
        self.scale = scale;
    }

    /// Current note phase.
    pub fn phase(&self) -> NotePhase {
        self.phase
    }

    /// MIDI note the correction targets.
    pub fn target(&self) -> i32 {
        self.target
    }

    /// Whether a note is sounding (onset or steady).
    pub fn voiced(&self) -> bool {
        matches!(self.phase, NotePhase::Onset | NotePhase::Steady)
    }

    fn start_note(&mut self, midi: f64) {
        self.target = snap_to_scale(midi, self.key, self.scale);
        self.phase = NotePhase::Onset;
        self.phase_frames = 0;
        self.jump_frames = 0;
    }

    /// Advance the state machine with one MIDI estimate.
    pub fn update(&mut self, midi: Option<f64>) {
        let Some(midi) = midi else {
            self.valid_run = 0;
            self.invalid_run += 1;
            match self.phase {
                NotePhase::Onset | NotePhase::Steady => {
                    if self.invalid_run >= RELEASE_INVALID_FRAMES {
                        self.phase = NotePhase::Release;
                        self.phase_frames = 0;
                        self.released_target = self.target;
                    }
                }
                NotePhase::Release => {
                    self.phase_frames += 1;
                    if self.phase_frames >= RELEASE_HOLD_FRAMES {
                        self.phase = NotePhase::Idle;
                        self.phase_frames = 0;
                    }
                }
                NotePhase::Idle => {}
            }
            return;
        };

        self.invalid_run = 0;
        self.valid_run += 1;
        match self.phase {
            NotePhase::Idle => {
                if self.valid_run >= ONSET_VALID_FRAMES {
                    self.start_note(midi);
                }
            }
            NotePhase::Release => {
                self.target = self.released_target;
                let deviation = (midi - f64::from(self.target)) * 100.0;
                if deviation.abs() <= SWITCH_CENTS {
                    self.phase = NotePhase::Steady;
                    self.phase_frames = 0;
                } else {
                    self.start_note(midi);
                }
            }
            NotePhase::Onset => {
                self.phase_frames += 1;
                let deviation = (midi - f64::from(self.target)) * 100.0;
                if deviation.abs() > RETRIGGER_CENTS {
                    self.jump_frames += 1;
                    if self.jump_frames >= RETRIGGER_FRAMES {
                        self.start_note(midi);
                    }
                } else {
                    self.jump_frames = 0;
                }
                if self.phase_frames >= ONSET_HOLD_FRAMES {
                    self.phase = NotePhase::Steady;
                    self.phase_frames = 0;
                }
            }
            NotePhase::Steady => {
                let deviation = (midi - f64::from(self.target)) * 100.0;
                if deviation.abs() > RETRIGGER_CENTS {
                    self.jump_frames += 1;
                    if self.jump_frames >= RETRIGGER_FRAMES {
                        self.start_note(midi);
                    }
                } else if deviation.abs() > SWITCH_CENTS {
                    self.jump_frames = 0;
                    let candidate = snap_to_scale(midi, self.key, self.scale);
                    if candidate != self.target {
                        self.start_note(midi);
                    }
                } else {
                    self.jump_frames = 0;
                }
            }
        }
    }

    /// Return to idle and forget the tracked note.
    pub fn reset(&mut self) {
        let key = self.key;
        let scale = self.scale;
        *self = Self::new(key, scale);
    }
}

/// Full-range output delay; the detection window is the same 40 ms.
pub const FULL_DELAY_MS: f64 = 40.0;
/// Low-latency output delay and window.
pub const LOW_DELAY_MS: f64 = 20.0;
/// Fastest retune time.
pub const MIN_RETUNE_MS: f64 = 10.0;
/// Slowest retune time.
pub const MAX_RETUNE_MS: f64 = 300.0;
pub use oc_device_schema::{KEY_NAMES, TUNER_MODES};

const DETECT_HOP_S: f64 = 0.005;
const VOICED_TAU_S: f64 = 0.003;
const MAX_GRAINS: usize = 8;
const MIN_AMOUNT: f64 = 1e-6;

/// Whether a parameter id names the enable switch.
pub fn switch_param(name: &str) -> bool {
    name == "enabled"
}

/// Map a key label to its pitch class.
pub fn key_name(name: &str) -> Option<f64> {
    let name = name.trim().to_ascii_uppercase();
    let index = match name.as_str() {
        "C" => 0,
        "C#" | "DB" => 1,
        "D" => 2,
        "D#" | "EB" => 3,
        "E" => 4,
        "F" => 5,
        "F#" | "GB" => 6,
        "G" => 7,
        "G#" | "AB" => 8,
        "A" => 9,
        "A#" | "BB" => 10,
        "B" => 11,
        _ => return None,
    };
    Some(index as f64)
}

/// Map a mode label to its selector code.
pub fn mode_code(name: &str) -> Option<f64> {
    let name = name.trim().to_ascii_lowercase().replace(['-', '_'], " ");
    match name.as_str() {
        "full range" | "full" => Some(0.0),
        "low latency" | "low" => Some(1.0),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Grain {
    /// Input sample at the centre of the analysis window.
    centre: i64,
    /// Output sample the centre is placed at.
    epoch: i64,
    radius: usize,
    /// Next window index still to contribute.
    cursor: i64,
}

/// Pitch corrector: detection, note state, scale snap and PSOLA resynthesis.
pub struct Tuner {
    sample_rate: u32,
    low_latency: bool,
    amount: f64,
    retune_ms: f64,
    retune_coeff: f64,
    key: u8,
    scale: Scale,
    enabled: bool,

    decimator: Decimator,
    detector_full: YinDetector,
    detector_low: YinDetector,
    detector_rate: f64,
    tracker: PeriodTracker,
    notes: NoteTracker,
    detect_hop: usize,
    detect_counter: usize,

    delay: usize,
    period: f64,
    correction: f64,
    ratio: f64,
    voiced_gain: f64,
    voiced_coeff: f64,

    dry_l: Vec<f32>,
    dry_r: Vec<f32>,
    dry_mask: usize,
    smooth: Vec<f64>,
    smooth_mask: usize,
    acc_l: Vec<f64>,
    acc_r: Vec<f64>,
    acc_w: Vec<f64>,
    acc_mask: usize,

    anchor: f64,
    anchor_ready: bool,
    epoch_target: f64,
    epoch_ready: bool,
    grains: [Grain; MAX_GRAINS],
    grain_count: usize,
    write: u64,
    active_last: bool,
}

impl Tuner {
    /// Prepare the detector, note tracker and resynthesis buffers.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let decimator = Decimator::new(sample_rate);
        let detector_rate = decimator.rate();
        let detector_full = YinDetector::new(detector_rate, FULL_WINDOW_S, MIN_F0);
        let detector_low = YinDetector::new(detector_rate, LOW_WINDOW_S, LOW_MIN_F0);
        let detect_hop = ((sample_rate as f64 * DETECT_HOP_S).round() as usize).max(1);
        let delay = ((sample_rate as f64 * FULL_DELAY_MS * 0.001).round() as usize).max(2);
        let dry_len = (delay + 2).next_power_of_two();
        let longest_period = (sample_rate as f64 / 75.0).ceil() as usize + 2;
        let smooth_len = (2 * longest_period + 8).next_power_of_two();
        let acc_len = (delay + longest_period + 8).next_power_of_two();
        let voiced_coeff = 1.0 - (-1.0 / (VOICED_TAU_S * sample_rate as f64)).exp();
        let mut tuner = Self {
            sample_rate,
            low_latency: false,
            amount: 1.0,
            retune_ms: 50.0,
            retune_coeff: 0.0,
            key: 0,
            scale: Scale::Chromatic,
            enabled: true,
            decimator,
            detector_full,
            detector_low,
            detector_rate,
            tracker: PeriodTracker::default(),
            notes: NoteTracker::new(0, Scale::Chromatic),
            detect_hop,
            detect_counter: 0,
            delay,
            period: 0.0,
            correction: 0.0,
            ratio: 1.0,
            voiced_gain: 0.0,
            voiced_coeff,
            dry_l: vec![0.0; dry_len],
            dry_r: vec![0.0; dry_len],
            dry_mask: dry_len - 1,
            smooth: vec![0.0; smooth_len],
            smooth_mask: smooth_len - 1,
            acc_l: vec![0.0; acc_len],
            acc_r: vec![0.0; acc_len],
            acc_w: vec![0.0; acc_len],
            acc_mask: acc_len - 1,
            anchor: 0.0,
            anchor_ready: false,
            epoch_target: 0.0,
            epoch_ready: false,
            grains: [Grain {
                centre: 0,
                epoch: 0,
                radius: 0,
                cursor: 0,
            }; MAX_GRAINS],
            grain_count: 0,
            write: 0,
            active_last: false,
        };
        tuner.update_retune();
        tuner
    }

    /// Bytes of analysis and grain buffers held by the tuner.
    pub fn memory_bytes(&self) -> usize {
        let complex = |len: usize| len * std::mem::size_of::<f64>();
        self.dry_l.len() * 4
            + self.dry_r.len() * 4
            + complex(self.smooth.len())
            + complex(self.detector_full.frame.len())
            + complex(self.detector_low.frame.len())
            + complex(self.detector_full.difference.len())
            + complex(self.detector_low.difference.len())
            + 3 * complex(self.acc_l.len())
    }

    /// Current correction in cents, positive for a pitch lift.
    pub fn correction_cents(&self) -> f64 {
        self.correction
    }

    /// Whether correction is enabled and has a voiced note.
    pub fn active(&self) -> bool {
        self.enabled && self.amount > MIN_AMOUNT
    }

    fn update_retune(&mut self) {
        let period = self.detect_hop as f64 / self.sample_rate as f64;
        let tau = (self.retune_ms * 0.001).max(1e-4);
        self.retune_coeff = 1.0 - (-period / tau).exp();
    }

    fn configure_mode(&mut self) {
        let delay_ms = if self.low_latency {
            LOW_DELAY_MS
        } else {
            FULL_DELAY_MS
        };
        self.delay = ((self.sample_rate as f64 * delay_ms * 0.001).round() as usize)
            .clamp(2, self.dry_l.len() - 1);
        self.reset_resynthesis();
    }

    fn reset_resynthesis(&mut self) {
        self.grains.fill(Grain {
            centre: 0,
            epoch: 0,
            radius: 0,
            cursor: 0,
        });
        self.acc_l.fill(0.0);
        self.acc_r.fill(0.0);
        self.acc_w.fill(0.0);
        self.grain_count = 0;
        self.epoch_ready = false;
        self.epoch_target = 0.0;
        self.anchor_ready = false;
        self.voiced_gain = 0.0;
        self.correction = 0.0;
        self.ratio = 1.0;
        self.period = 0.0;
        self.tracker.reset();
        self.notes.reset();
    }

    fn radius(&self) -> usize {
        psola::grain_radius(self.period, self.period, 1.0)
    }

    fn smoothed(&self, index: u64) -> f64 {
        let mask = self.smooth_mask;
        let mut sum = 0.0;
        let mut cursor = index as usize;
        for _ in 0..5 {
            sum += self.smooth[cursor & mask];
            cursor = cursor.wrapping_sub(1);
        }
        sum * 0.2
    }

    fn observe_peak(&mut self, index: u64) {
        let period = self.period;
        if period < 6.0 {
            return;
        }
        let span = period as u64;
        if index < span + 2 {
            return;
        }
        let start = index - span;
        let mut best = f64::NEG_INFINITY;
        let mut best_index = 0;
        for cursor in start + 1..index {
            let value = self.smoothed(cursor);
            if value > best
                && value > self.smoothed(cursor - 1)
                && value >= self.smoothed(cursor + 1)
            {
                best = value;
                best_index = cursor;
            }
        }
        if best_index == 0 || best <= 1e-5 {
            return;
        }
        let left = self.smoothed(best_index - 1);
        let right = self.smoothed(best_index + 1);
        let offset = psola::parabolic_offset(left, best, right);
        self.anchor = best_index as f64 + offset;
        self.anchor_ready = true;
    }

    fn ingest(&mut self, left: f32, right: f32, detect: bool) {
        let index = (self.write as usize) & self.dry_mask;
        self.dry_l[index] = left;
        self.dry_r[index] = right;
        let mid = 0.5 * (left as f64 + right as f64);
        self.smooth[index & self.smooth_mask] = mid;
        if let Some(decimated) = self.decimator.push(mid) {
            if self.low_latency {
                self.detector_low.push(decimated);
            } else {
                self.detector_full.push(decimated);
            }
        }
        if !detect {
            self.detect_counter = 0;
            return;
        }
        self.detect_counter += 1;
        if self.detect_counter >= self.detect_hop {
            self.detect_counter = 0;
            self.detect_tick();
        }
    }

    fn detect_tick(&mut self) {
        let estimate = if self.low_latency {
            self.detector_low.detect()
        } else {
            self.detector_full.detect()
        };
        self.tracker.update(estimate, self.detector_rate);
        let midi = if self.tracker.valid {
            let f0 = self.tracker.f0;
            let (low, high) = if self.low_latency {
                (LOW_MIN_F0, MAX_F0)
            } else {
                (MIN_F0, MAX_F0)
            };
            if f0 >= low && f0 <= high {
                Some(69.0 + 12.0 * (f0 / 440.0).log2())
            } else {
                None
            }
        } else {
            None
        };
        self.notes.update(midi);
        self.period = self.tracker.period * self.decimator.factor() as f64;
        let desired = match midi {
            Some(midi) if self.notes.voiced() => {
                self.amount * (f64::from(self.notes.target()) - midi) * 100.0
            }
            _ => 0.0,
        };
        self.correction += (desired - self.correction) * self.retune_coeff;
        self.correction = self.correction.clamp(-1_200.0, 1_200.0);
        self.ratio = psola::cents_ratio(self.correction);
    }

    fn grain_at(&self, epoch: f64) -> (i64, i64) {
        let target = epoch - self.delay as f64;
        let centre = if self.anchor_ready {
            let steps = ((target - self.anchor) / self.period).round();
            self.anchor + steps * self.period
        } else {
            target
        };
        (centre.round() as i64, epoch.round() as i64)
    }

    fn add_grain_sample(&mut self, centre: i64, epoch: i64, radius: usize, offset: i64) {
        let source = centre - radius as i64 + offset;
        if source < 0 || source > self.write as i64 {
            return;
        }
        let output = epoch - radius as i64 + offset;
        if output < 0 {
            return;
        }
        let sample_l = self.dry_l[(source as usize) & self.dry_mask];
        let sample_r = self.dry_r[(source as usize) & self.dry_mask];
        let window = psola::grain_window(offset as f64, radius);
        let slot = (output as usize) & self.acc_mask;
        self.acc_l[slot] += f64::from(sample_l) * window;
        self.acc_r[slot] += f64::from(sample_r) * window;
        self.acc_w[slot] += window;
    }

    fn schedule_grains(&mut self) {
        if self.period <= 0.0 || !self.notes.voiced() {
            self.epoch_ready = false;
            self.anchor_ready = false;
            return;
        }
        let radius = self.radius().clamp(2, self.delay / 2);
        if !self.epoch_ready {
            self.epoch_target = self.write as f64 + radius as f64;
            self.epoch_ready = true;
        }
        let index = self.write as i64;
        let mut created = 0;
        while created < MAX_GRAINS {
            let (centre, epoch) = self.grain_at(self.epoch_target);
            let first = centre - radius as i64;
            if first > index {
                break;
            }
            let last = (index - first).min(2 * radius as i64 - 1);
            for offset in 0..=last {
                self.add_grain_sample(centre, epoch, radius, offset);
            }
            if last + 1 < 2 * radius as i64 {
                if self.grain_count == MAX_GRAINS {
                    self.grains.copy_within(1.., 0);
                    self.grain_count -= 1;
                }
                self.grains[self.grain_count] = Grain {
                    centre,
                    epoch,
                    radius,
                    cursor: last + 1,
                };
                self.grain_count += 1;
            }
            self.epoch_target += psola::synth_hop(self.period, self.ratio);
            created += 1;
        }
    }

    fn resynthesise(&mut self) -> (f32, f32) {
        let index = self.write as i64;
        let mut cursor = 0;
        while cursor < self.grain_count {
            let mut grain = self.grains[cursor];
            if grain.cursor as usize >= 2 * grain.radius {
                self.grains
                    .copy_within(cursor + 1..self.grain_count, cursor);
                self.grain_count -= 1;
                continue;
            }
            let source = grain.centre - grain.radius as i64 + grain.cursor;
            if source <= index {
                self.add_grain_sample(grain.centre, grain.epoch, grain.radius, grain.cursor);
                grain.cursor += 1;
                if grain.cursor as usize >= 2 * grain.radius {
                    self.grains
                        .copy_within(cursor + 1..self.grain_count, cursor);
                    self.grain_count -= 1;
                    continue;
                }
                self.grains[cursor] = grain;
            }
            cursor += 1;
        }
        let slot = (index as usize) & self.acc_mask;
        let weight = self.acc_w[slot];
        let (wet_l, wet_r) = if weight > 1e-6 {
            (self.acc_l[slot] / weight, self.acc_r[slot] / weight)
        } else {
            (0.0, 0.0)
        };
        self.acc_l[slot] = 0.0;
        self.acc_r[slot] = 0.0;
        self.acc_w[slot] = 0.0;
        let dry_index = (self.write as usize).wrapping_sub(self.delay) & self.dry_mask;
        let (dry_l, dry_r) = if self.write as usize >= self.delay {
            (self.dry_l[dry_index] as f64, self.dry_r[dry_index] as f64)
        } else {
            (0.0, 0.0)
        };
        let coverage = weight.clamp(0.0, 1.0);
        let mix = self.voiced_gain * coverage;
        (
            (mix * wet_l + (1.0 - mix) * dry_l) as f32,
            (mix * wet_r + (1.0 - mix) * dry_r) as f32,
        )
    }

    fn process_sample(&mut self, left: f32, right: f32) -> (f32, f32) {
        self.ingest(left, right, true);
        let time = self.write as f64 - self.delay as f64;
        self.voiced_gain +=
            (if self.notes.voiced() { 1.0 } else { 0.0 } - self.voiced_gain) * self.voiced_coeff;
        if time >= 0.0 {
            if self.notes.voiced() && self.period > 0.0 {
                let due =
                    !self.anchor_ready || self.write as f64 - self.anchor >= 0.75 * self.period;
                if due {
                    self.observe_peak(self.write);
                }
            }
            self.schedule_grains();
        }
        let output = self.resynthesise();
        self.write += 1;
        output
    }
}

impl AudioEffect for Tuner {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let active = self.active();
        if active != self.active_last {
            self.reset_resynthesis();
            self.active_last = active;
        }
        if !active {
            for frame in buffer.as_chunks_mut::<2>().0 {
                self.ingest(frame[0], frame[1], false);
                self.write += 1;
            }
            return;
        }
        for frame in buffer.as_chunks_mut::<2>().0 {
            let (left, right) = self.process_sample(frame[0], frame[1]);
            frame[0] = left;
            frame[1] = right;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "amount" => self.amount = value.clamp(0.0, 1.0),
            "retune" => {
                self.retune_ms = value.clamp(MIN_RETUNE_MS, MAX_RETUNE_MS);
                self.update_retune();
            }
            "key" => {
                self.key = (value.round() as i64).rem_euclid(12) as u8;
                self.notes.set_scale(self.key, self.scale);
            }
            "scale" => {
                self.scale = Scale::from_index(value.round().max(0.0) as usize);
                self.notes.set_scale(self.key, self.scale);
            }
            "mode" => {
                let low = value >= 0.5;
                if low != self.low_latency {
                    self.low_latency = low;
                    self.configure_mode();
                }
            }
            "enabled" => self.enabled = value >= 0.5,
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "amount" => Some(self.amount),
            "retune" => Some(self.retune_ms),
            "key" => Some(f64::from(self.key)),
            "scale" => Some(self.scale.index() as f64),
            "mode" => Some(if self.low_latency { 1.0 } else { 0.0 }),
            "enabled" => Some(if self.enabled { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.dry_l.fill(0.0);
        self.dry_r.fill(0.0);
        self.smooth.fill(0.0);
        self.decimator.reset();
        self.detector_full.reset();
        self.detector_low.reset();
        self.write = 0;
        self.detect_counter = 0;
        self.active_last = false;
        self.reset_resynthesis();
    }

    fn latency_samples(&self) -> u32 {
        if self.active() { self.delay as u32 } else { 0 }
    }

    fn name(&self) -> &str {
        "tuner"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(f0: f64, amplitude: f64, seconds: f64, rate: u32) -> Vec<f64> {
        let frames = (rate as f64 * seconds) as usize;
        (0..frames)
            .map(|index| {
                amplitude * (TAU * f0 * index as f64 / rate as f64).sin()
                    + 0.5 * amplitude * (2.0 * TAU * f0 * index as f64 / rate as f64).sin()
            })
            .collect()
    }

    fn detect_track(samples: &[f64], rate: u32) -> Vec<(f64, f64)> {
        let mut decimator = Decimator::new(rate);
        let mut detector = YinDetector::new(decimator.rate(), FULL_WINDOW_S, MIN_F0);
        let hop = ((decimator.rate() * 0.005).round() as usize).max(1);
        let mut tracker = PeriodTracker::default();
        let mut track = Vec::new();
        let mut pushed = 0;
        for sample in samples {
            if let Some(value) = decimator.push(*sample) {
                detector.push(value);
                pushed += 1;
                if pushed % hop == 0
                    && let Some(estimate) = detector.detect()
                {
                    tracker.update(Some(estimate), decimator.rate());
                    track.push((tracker.f0, tracker.confidence));
                }
            }
        }
        track
    }

    fn cents(measured: f64, reference: f64) -> f64 {
        1_200.0 * (measured / reference).log2()
    }

    #[test]
    fn pitch_error_across_the_range() {
        let mut worst = 0.0_f64;
        for f0 in [
            80.0, 100.0, 150.0, 220.0, 330.0, 440.0, 660.0, 880.0, 1_000.0,
        ] {
            let samples = tone(f0, 0.3, 1.0, RATE);
            let track = detect_track(&samples, RATE);
            let settled = &track[track.len() / 2..];
            let measured: f64 =
                settled.iter().map(|(f0, _)| *f0).sum::<f64>() / settled.len() as f64;
            let error = cents(measured, f0);
            println!("{f0:>6.0} Hz: {measured:>9.3} Hz, {error:+.4} cents");
            worst = worst.max(error.abs());
        }
        assert!(worst < 2.0, "worst pitch error {worst:.3} cents");
    }

    #[test]
    fn pitch_error_holds_at_other_sample_rates() {
        for rate in [44_100, 48_000, 96_000] {
            let samples = tone(220.0, 0.3, 1.0, rate);
            let track = detect_track(&samples, rate);
            let settled = &track[track.len() / 2..];
            let measured: f64 =
                settled.iter().map(|(f0, _)| *f0).sum::<f64>() / settled.len() as f64;
            let error = cents(measured, 220.0);
            println!("{rate} Hz output: {measured:.3} Hz, {error:+.4} cents");
            assert!(error.abs() < 2.0, "{rate} Hz: {error:.3} cents");
        }
    }

    #[test]
    fn breathy_noise_never_reports_an_octave_up() {
        let mut noise = 0x1234_5678_u32;
        for snr_db in [12.0, 6.0] {
            let samples: Vec<f64> = tone(220.0, 0.3, 1.5, RATE)
                .into_iter()
                .map(|sample| {
                    noise ^= noise << 13;
                    noise ^= noise >> 17;
                    noise ^= noise << 5;
                    let white = (noise as f64 / u32::MAX as f64) * 2.0 - 1.0;
                    sample + white * 0.3 * 10f64.powf(-snr_db / 20.0)
                })
                .collect();
            let track = detect_track(&samples, RATE);
            let valid: Vec<f64> = track
                .iter()
                .map(|(f0, _)| *f0)
                .filter(|f0| *f0 > 0.0)
                .collect();
            let octave_up = valid.iter().filter(|f0| **f0 > 330.0).count();
            let median_error = cents(valid[valid.len() / 2], 220.0);
            println!(
                "snr {snr_db} dB: {} valid frames, {octave_up} octave-up, median {median_error:+.2} cents",
                valid.len()
            );
            assert_eq!(octave_up, 0, "snr {snr_db} dB reported an octave up");
            assert!(
                median_error.abs() < 20.0,
                "snr {snr_db} dB median error {median_error:.2} cents"
            );
        }
    }

    #[test]
    fn a_single_octave_jump_is_filtered_out() {
        let mut tracker = PeriodTracker::default();
        for _ in 0..8 {
            tracker.update(Some((200.0, 0.9)), ANALYSIS_RATE);
        }
        tracker.update(Some((100.0, 0.9)), ANALYSIS_RATE);
        assert!(
            (tracker.period - 200.0).abs() < 1.0,
            "a lone octave-up frame moved the track to {}",
            tracker.period
        );
        for _ in 0..4 {
            tracker.update(Some((200.0, 0.9)), ANALYSIS_RATE);
        }
        tracker.update(Some((400.0, 0.9)), ANALYSIS_RATE);
        assert!(
            (tracker.period - 200.0).abs() < 1.0,
            "a lone octave-down frame moved the track to {}",
            tracker.period
        );
    }

    #[test]
    fn a_short_gap_keeps_the_track_and_a_long_gap_clears_it() {
        let mut tracker = PeriodTracker::default();
        for _ in 0..8 {
            tracker.update(Some((200.0, 0.9)), ANALYSIS_RATE);
        }
        tracker.update(None, ANALYSIS_RATE);
        tracker.update(Some((202.0, 0.9)), ANALYSIS_RATE);
        assert!(tracker.valid);
        assert!((tracker.period - 201.0).abs() < 2.0);
        tracker.update(None, ANALYSIS_RATE);
        tracker.update(None, ANALYSIS_RATE);
        tracker.update(None, ANALYSIS_RATE);
        assert!(!tracker.valid);
    }

    #[test]
    fn scale_snap_uses_the_whole_scale() {
        let cases = [
            (Scale::Chromatic, 60.2, 60),
            (Scale::Major, 60.0, 60),
            (Scale::Major, 61.4, 62),
            (Scale::Major, 61.6, 62),
            (Scale::Major, 63.0, 62),
            (Scale::Major, 63.5, 64),
            (Scale::MajorPentatonic, 61.8, 62),
            (Scale::MajorPentatonic, 62.2, 62),
            (Scale::NaturalMinor, 63.6, 63),
            (Scale::Blues, 63.6, 63),
        ];
        for (scale, midi, expected) in cases {
            assert_eq!(
                snap_to_scale(midi, 0, scale),
                expected,
                "{scale:?} at {midi}"
            );
        }
        // Every scale is reachable by name and index.
        for (index, name) in Scale::NAMES.iter().enumerate() {
            assert_eq!(Scale::from_name(name).unwrap().index(), index);
            assert_eq!(
                Scale::from_name(&name.to_lowercase()).unwrap().index(),
                index
            );
        }
        assert_eq!(Scale::from_name("C major"), None);
        assert_eq!(Scale::from_index(99), Scale::Chromatic);
    }

    #[test]
    fn c_major_run_snaps_each_note_within_five_cents() {
        let mut tracker = NoteTracker::new(0, Scale::Major);
        let notes = [60, 62, 64, 65, 67, 69, 71, 72];
        for note in notes {
            for detune in [-40.0, -25.0, 0.0, 25.0, 40.0] {
                tracker.reset();
                let midi = f64::from(note) + detune / 100.0;
                for _ in 0..8 {
                    tracker.update(Some(midi));
                }
                assert_eq!(tracker.target(), note, "note {note} detune {detune}");
                let correction = (f64::from(tracker.target()) - midi) * 100.0;
                assert!(
                    correction.abs() <= 50.0,
                    "note {note} detune {detune} asked for {correction:.1} cents"
                );
                assert_eq!(tracker.phase(), NotePhase::Steady);
            }
        }
    }

    #[test]
    fn vibrato_never_retriggers_a_note() {
        let mut tracker = NoteTracker::new(0, Scale::Major);
        for frame in 0..200 {
            let time = frame as f64 * 0.005;
            let midi = 64.0 + 0.30 + 0.25 * (TAU * 6.0 * time).sin();
            tracker.update(Some(midi));
        }
        assert_eq!(tracker.target(), 64);
        assert_eq!(tracker.phase(), NotePhase::Steady);
    }

    #[test]
    fn a_legato_step_starts_a_new_note() {
        let mut tracker = NoteTracker::new(0, Scale::Major);
        for _ in 0..20 {
            tracker.update(Some(64.0));
        }
        assert_eq!(tracker.phase(), NotePhase::Steady);
        for _ in 0..3 {
            tracker.update(Some(66.2));
        }
        assert_eq!(tracker.target(), 67);
        assert_eq!(tracker.phase(), NotePhase::Onset);
        for _ in 0..6 {
            tracker.update(Some(67.0));
        }
        assert_eq!(tracker.phase(), NotePhase::Steady);
    }

    #[test]
    fn the_hysteresis_band_holds_a_note_before_switching() {
        let mut tracker = NoteTracker::new(0, Scale::Major);
        for _ in 0..10 {
            tracker.update(Some(64.0));
        }
        for _ in 0..10 {
            tracker.update(Some(64.55));
        }
        assert_eq!(tracker.target(), 64, "55 cents is inside the band");
        for _ in 0..10 {
            tracker.update(Some(64.65));
        }
        assert_eq!(tracker.target(), 65, "65 cents is outside the band");
    }

    #[test]
    fn release_needs_two_invalid_frames_and_returns_to_idle() {
        let mut tracker = NoteTracker::new(0, Scale::Major);
        for _ in 0..10 {
            tracker.update(Some(64.0));
        }
        tracker.update(None);
        assert_eq!(tracker.phase(), NotePhase::Steady);
        tracker.update(None);
        assert_eq!(tracker.phase(), NotePhase::Release);
        tracker.update(Some(64.0));
        assert_eq!(tracker.phase(), NotePhase::Steady);
        assert_eq!(tracker.target(), 64);
        for _ in 0..2 {
            tracker.update(None);
        }
        for _ in 0..10 {
            tracker.update(None);
        }
        assert_eq!(tracker.phase(), NotePhase::Idle);
        // Idle needs two valid frames before a new onset.
        tracker.update(Some(67.0));
        assert_eq!(tracker.phase(), NotePhase::Idle);
        tracker.update(Some(67.0));
        assert_eq!(tracker.phase(), NotePhase::Onset);
        assert_eq!(tracker.target(), 67);
    }

    fn sine(f0: f64, amplitude: f64, seconds: f64) -> Vec<f64> {
        let frames = (RATE as f64 * seconds) as usize;
        (0..frames)
            .map(|index| amplitude * (TAU * f0 * index as f64 / RATE as f64).sin())
            .collect()
    }

    fn vibrato_tone(f0: f64, depth_cents: f64, rate_hz: f64, seconds: f64) -> Vec<f64> {
        let frames = (RATE as f64 * seconds) as usize;
        let mut phase = 0.0;
        let mut output = Vec::with_capacity(frames);
        for index in 0..frames {
            let time = index as f64 / RATE as f64;
            let frequency = f0 * 2f64.powf(depth_cents * (TAU * rate_hz * time).sin() / 1_200.0);
            phase += TAU * frequency / RATE as f64;
            output.push(0.3 * phase.sin());
        }
        output
    }

    fn stereo(samples: &[f64]) -> Vec<f32> {
        let mut buffer = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            buffer.push(*sample as f32);
            buffer.push(*sample as f32);
        }
        buffer
    }

    fn run(effect: &mut Tuner, input: &[f32], frames: usize) -> Vec<f32> {
        let mut output = input.to_vec();
        for chunk in output.chunks_mut(frames * 2) {
            effect.process(chunk, RATE);
        }
        output
    }

    fn left(samples: &[f32]) -> Vec<f32> {
        samples.iter().step_by(2).copied().collect()
    }

    fn crossings(samples: &[f32], start: usize, end: usize) -> Vec<f64> {
        let mut times = Vec::new();
        for index in start.max(1)..end.min(samples.len()) {
            let previous = samples[index - 1];
            let current = samples[index];
            if previous < 0.0 && current >= 0.0 {
                let fraction = f64::from(-previous) / f64::from(current - previous);
                times.push((index - 1) as f64 + fraction);
            }
        }
        times
    }

    fn frequency(samples: &[f32], start: usize, end: usize) -> f64 {
        let times = crossings(samples, start, end);
        if times.len() < 3 {
            return 0.0;
        }
        let cycles = (times.len() - 1) as f64;
        cycles * RATE as f64 / (times[times.len() - 1] - times[0])
    }

    fn vibrato_depth(samples: &[f32], start: usize, end: usize) -> f64 {
        let times = crossings(samples, start, end);
        if times.len() < 4 {
            return 0.0;
        }
        let mut lowest: f64 = f64::INFINITY;
        let mut highest: f64 = f64::NEG_INFINITY;
        for pair in times.windows(2) {
            let f0 = RATE as f64 / (pair[1] - pair[0]);
            let deviation = 1_200.0 * (f0 / 220.0).log2();
            lowest = lowest.min(deviation);
            highest = highest.max(deviation);
        }
        (highest - lowest) * 0.5
    }

    fn max_step(samples: &[f32], start: usize) -> f64 {
        let mut worst = 0.0_f64;
        for index in start.max(1)..samples.len() {
            worst = worst.max((samples[index] - samples[index - 1]).abs() as f64);
        }
        worst
    }

    fn tuned(amount: f64) -> Tuner {
        let mut effect = Tuner::new(RATE);
        effect.set_param("key", 0.0);
        effect.set_param("scale", Scale::Major.index() as f64);
        effect.set_param("retune", 30.0);
        effect.set_param("amount", amount);
        effect
    }

    #[test]
    fn a_hundred_cent_detuned_220_hz_is_corrected_within_two_cents() {
        // A major pentatonic has no G/G#, so a 100 cent miss from A is unambiguous.
        let mut effect = tuned(1.0);
        effect.set_param("key", 9.0);
        effect.set_param("scale", Scale::MajorPentatonic.index() as f64);
        let flat = 220.0 * 2f64.powf(-100.0 / 1_200.0);
        let input = stereo(&sine(flat, 0.3, 2.0));
        let output = run(&mut effect, &input, 480);
        let delay = RATE as usize * 4 / 100;
        let measured = frequency(
            &left(&output),
            delay + RATE as usize * 3 / 5,
            delay + RATE as usize * 8 / 5,
        );
        let error = 1_200.0 * (measured / 220.0).log2();
        assert!(error.abs() < 2.0, "{error:.3} cents off");
    }

    #[test]
    fn amount_zero_is_a_bit_exact_bypass() {
        let mut effect = tuned(0.0);
        let input = stereo(&sine(220.0, 0.3, 1.0));
        let output = run(&mut effect, &input, 480);
        assert_eq!(output, input);
        assert_eq!(effect.latency_samples(), 0);
    }

    #[test]
    fn a_c_major_run_lands_on_the_scale_notes() {
        let mut effect = tuned(1.0);
        effect.set_param("key", 0.0);
        effect.set_param("scale", Scale::Major.index() as f64);
        let notes = [60.0, 62.0, 64.0, 65.0, 67.0, 69.0, 71.0, 72.0];
        let mut input = Vec::new();
        let note_seconds = 0.4;
        for note in notes {
            let hertz = 440.0 * 2f64.powf((note - 69.0) / 12.0) * 2f64.powf(-40.0 / 1_200.0);
            input.extend(sine(hertz, 0.3, note_seconds));
            input.extend(std::iter::repeat_n(0.0, (RATE as f64 * 0.03) as usize));
        }
        let input = stereo(&input);
        let output = left(&run(&mut effect, &input, 480));
        let delay = RATE as usize * 4 / 100;
        let stride = (RATE as f64 * note_seconds) as usize + (RATE as f64 * 0.03) as usize;
        let mut worst = 0.0_f64;
        for (index, note) in notes.iter().enumerate() {
            let expected = 440.0 * 2f64.powf((note - 69.0) / 12.0);
            let start = delay + index * stride + (RATE as f64 * 0.25) as usize;
            let end = delay + index * stride + (RATE as f64 * 0.38) as usize;
            let measured = frequency(&output, start, end);
            let error = 1_200.0 * (measured / expected).log2();
            println!("note {note:>3.0}: {measured:>8.3} Hz, {error:+.3} cents");
            worst = worst.max(error.abs());
        }
        assert!(worst < 5.0, "worst scale error {worst:.3} cents");
    }

    #[test]
    fn vibrato_survives_half_amount() {
        let mut effect = tuned(0.5);
        let input = vibrato_tone(220.0, 25.0, 6.0, 2.0);
        let depth_in = vibrato_depth(&left(&stereo(&input)), 0, input.len());
        let output = left(&run(&mut effect, &stereo(&input), 480));
        let delay = RATE as usize * 4 / 100;
        let depth = vibrato_depth(
            &output,
            delay + RATE as usize * 3 / 5,
            delay + RATE as usize * 8 / 5,
        );
        println!("vibrato: input {depth_in:.2} cents, output {depth:.2} cents");
        assert!(
            depth < depth_in - 1.0 && depth > 8.0,
            "vibrato depth {depth:.2} cents"
        );
    }

    #[test]
    fn hard_tune_steps_do_not_click() {
        let mut effect = tuned(1.0);
        effect.set_param("retune", 10.0);
        let frames = RATE as usize * 2;
        let mut input = Vec::with_capacity(frames);
        let mut phase = 0.0;
        for index in 0..frames {
            let frequency = match index {
                0..=48_000 => 220.0,
                48_001..=72_000 => 246.94,
                _ => 207.65,
            };
            phase += TAU * frequency / RATE as f64;
            input.push(0.3 * phase.sin());
        }
        let input = stereo(&input);
        let output = run(&mut effect, &input, 480);
        let delay = RATE as usize * 4 / 100;
        let input_step = max_step(&left(&input), 100);
        let output_step = max_step(&left(&output), delay + 100);
        assert!(
            output_step < input_step * 2.0 + 1e-4,
            "output stepped {output_step:.5} against input {input_step:.5}"
        );
    }

    #[test]
    fn reported_latency_matches_the_measured_delay() {
        for (mode, expected) in [(0.0, 1_920usize), (1.0, 960)] {
            let mut effect = tuned(1.0);
            effect.set_param("mode", mode);
            assert_eq!(effect.latency_samples() as usize, expected);
            let frames = RATE as usize * 2;
            let mut mono = Vec::with_capacity(frames);
            for index in 0..frames {
                let time = index as f64 / RATE as f64;
                let envelope = 1.0 + 0.6 * (TAU * 7.0 * time).sin();
                mono.push(0.3 * envelope * (TAU * 220.0 * time).sin());
            }
            let input = stereo(&mono);
            let output = left(&run(&mut effect, &input, 480));
            let input = left(&input);
            let envelope = |samples: &[f32]| -> Vec<f64> {
                let mut smoothed = vec![0.0; samples.len()];
                let mut sum = 0.0;
                for index in 0..samples.len() {
                    sum += f64::from(samples[index].abs());
                    if index >= 240 {
                        sum -= f64::from(samples[index - 240].abs());
                    }
                    smoothed[index] = sum / 240.0;
                }
                smoothed
            };
            let dry = envelope(&input);
            let wet = envelope(&output);
            let start = RATE as usize / 2;
            let end = RATE as usize * 3 / 2;
            let mut best = 0usize;
            let mut best_score = f64::NEG_INFINITY;
            for lag in 0..3_000usize {
                let mut score = 0.0;
                for index in start..end {
                    score += dry[index] * wet[index + lag];
                }
                if score > best_score {
                    best_score = score;
                    best = lag;
                }
            }
            // PSOLA may place a grain up to half a period off the nominal map,
            // so the feature delay is D +- one grain phase.
            println!("mode {mode}: reported {expected}, measured {best}");
            assert!(
                best.abs_diff(expected) <= 140,
                "mode {mode}: reported {expected}, measured {best}"
            );
        }
    }

    #[test]
    fn the_dry_path_delays_by_exactly_the_reported_latency() {
        let mut effect = tuned(1.0);
        let frames = RATE as usize * 2;
        let mut mono = Vec::with_capacity(frames);
        for index in 0..frames {
            let time = index as f64 / RATE as f64;
            let envelope = 1.0 + 0.6 * (TAU * 7.0 * time).sin();
            mono.push(0.3 * envelope * (TAU * 40.0 * time).sin());
        }
        let input = stereo(&mono);
        let output = left(&run(&mut effect, &input, 480));
        let input = left(&input);
        let onset = input.iter().position(|sample| sample.abs() > 0.05).unwrap();
        let delayed = output
            .iter()
            .position(|sample| sample.abs() > 0.05)
            .unwrap();
        let delay = delayed - onset;
        println!(
            "dry onset delay: {delay} samples, reported {}",
            effect.latency_samples()
        );
        assert_eq!(delay, effect.latency_samples() as usize);
    }

    #[test]
    fn breathy_notes_do_not_jump_an_octave() {
        let mut effect = tuned(1.0);
        let mut noise = 0x9e37_79b9u32;
        let input: Vec<f64> = sine(220.0, 0.3, 2.0)
            .into_iter()
            .map(|sample| {
                noise ^= noise << 13;
                noise ^= noise >> 17;
                noise ^= noise << 5;
                let white = (noise as f64 / u32::MAX as f64) * 2.0 - 1.0;
                sample + white * 0.15
            })
            .collect();
        let input = stereo(&input);
        let mut output = input.clone();
        let mut worst = 0.0_f64;
        for chunk in output.chunks_mut(480 * 2) {
            effect.process(chunk, RATE);
            worst = worst.max(effect.correction_cents().abs());
        }
        let rms = (output
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum::<f64>()
            / output.len() as f64)
            .sqrt();
        println!("breathy: worst correction {worst:.1} cents, rms {rms:.4}");
        assert!(worst < 300.0, "correction reached {worst:.1} cents");
        assert!(rms > 0.01, "breathy note was gated");
    }

    #[test]
    fn processing_is_block_size_invariant() {
        let input = stereo(&sine(233.0, 0.25, 1.0));
        let mut fine = tuned(1.0);
        let first = run(&mut fine, &input, 97);
        let mut coarse = tuned(1.0);
        let second = run(&mut coarse, &input, 480);
        assert_eq!(first, second);
    }

    #[test]
    #[ignore]
    fn measurements_for_docs() {
        use std::time::Instant;

        println!("detector error, 48 kHz, 40 ms window, 16 kHz analysis");
        for f0 in [
            80.0, 100.0, 150.0, 220.0, 330.0, 440.0, 660.0, 880.0, 1_000.0,
        ] {
            let track = detect_track(&tone(f0, 0.3, 1.0, RATE), RATE);
            let settled = &track[track.len() / 2..];
            let measured: f64 =
                settled.iter().map(|(f0, _)| *f0).sum::<f64>() / settled.len() as f64;
            println!(
                "  {f0:>6.0} Hz -> {measured:>9.3} Hz, {:+.3} cents",
                cents(measured, f0)
            );
        }

        for snr_db in [12.0, 6.0] {
            let mut noise = 0x1234_5678_u32;
            let samples: Vec<f64> = tone(220.0, 0.3, 1.5, RATE)
                .into_iter()
                .map(|sample| {
                    noise ^= noise << 13;
                    noise ^= noise >> 17;
                    noise ^= noise << 5;
                    let white = (noise as f64 / u32::MAX as f64) * 2.0 - 1.0;
                    sample + white * 0.3 * 10f64.powf(-snr_db / 20.0)
                })
                .collect();
            let track = detect_track(&samples, RATE);
            let valid: Vec<f64> = track
                .iter()
                .map(|(f0, _)| *f0)
                .filter(|f0| *f0 > 0.0)
                .collect();
            let octave_up = valid.iter().filter(|f0| **f0 > 330.0).count();
            let median = valid[valid.len() / 2];
            println!(
                "  breathy {snr_db:>4.0} dB: {} frames, {octave_up} octave-up, median {:+.2} cents",
                valid.len(),
                cents(median, 220.0)
            );
        }

        let cover = |effect: &mut Tuner, input: &[f32]| run(effect, input, 480);

        let mut effect = tuned(1.0);
        effect.set_param("key", 9.0);
        effect.set_param("scale", Scale::MajorPentatonic.index() as f64);
        let flat = 220.0 * 2f64.powf(-100.0 / 1_200.0);
        let output = cover(&mut effect, &stereo(&sine(flat, 0.3, 2.0)));
        let delay = RATE as usize * 4 / 100;
        let measured = frequency(
            &left(&output),
            delay + RATE as usize * 3 / 5,
            delay + RATE as usize * 8 / 5,
        );
        println!(
            "correction -100 cents -> {measured:.3} Hz, {:+.3} cents",
            cents(measured, 220.0)
        );

        let mut effect = tuned(1.0);
        effect.set_param("key", 0.0);
        effect.set_param("scale", Scale::Major.index() as f64);
        let notes = [60.0, 62.0, 64.0, 65.0, 67.0, 69.0, 71.0, 72.0];
        let mut notes_input = Vec::new();
        for note in notes {
            let hertz = 440.0 * 2f64.powf((note - 69.0) / 12.0) * 2f64.powf(-40.0 / 1_200.0);
            notes_input.extend(sine(hertz, 0.3, 0.4));
            notes_input.extend(std::iter::repeat_n(0.0, (RATE as f64 * 0.03) as usize));
        }
        let notes_output = left(&cover(&mut effect, &stereo(&notes_input)));
        let stride = (RATE as f64 * 0.4) as usize + (RATE as f64 * 0.03) as usize;
        let mut worst: f64 = 0.0;
        for (index, note) in notes.iter().enumerate() {
            let expected = 440.0 * 2f64.powf((note - 69.0) / 12.0);
            let start = delay + index * stride + (RATE as f64 * 0.25) as usize;
            let end = delay + index * stride + (RATE as f64 * 0.38) as usize;
            let measured = frequency(&notes_output, start, end);
            worst = worst.max(cents(measured, expected).abs());
        }
        println!("C major -40 cents run: worst {worst:.3} cents");

        let mut effect = tuned(0.5);
        let vibrato = vibrato_tone(220.0, 25.0, 6.0, 2.0);
        let depth_in = vibrato_depth(&left(&stereo(&vibrato)), 0, vibrato.len());
        let vibrato_out = left(&cover(&mut effect, &stereo(&vibrato)));
        let depth = vibrato_depth(
            &vibrato_out,
            delay + RATE as usize * 3 / 5,
            delay + RATE as usize * 8 / 5,
        );
        println!("vibrato 6 Hz +-25 cents at amount 0.5: {depth_in:.2} -> {depth:.2} cents");

        let mut effect = tuned(1.0);
        effect.set_param("retune", 10.0);
        let frames = RATE as usize * 2;
        let mut step_input = Vec::with_capacity(frames);
        let mut phase = 0.0;
        for index in 0..frames {
            let hertz = match index {
                0..=48_000 => 220.0,
                48_001..=72_000 => 246.94,
                _ => 207.65,
            };
            phase += TAU * hertz / RATE as f64;
            step_input.push(0.3 * phase.sin());
        }
        let step_output = cover(&mut effect, &stereo(&step_input));
        let input_step = max_step(&left(&stereo(&step_input)), 100);
        let output_step = max_step(&left(&step_output), delay + 100);
        println!(
            "hard 200/150 cent steps at 10 ms retune: max step {input_step:.5} -> {output_step:.5} ({:.3}x)",
            output_step / input_step
        );

        let mut effect = tuned(1.0);
        let f0 = 220.0;
        let hertz = f0 * 2f64.powf(-100.0 / 1_200.0);
        let mut cpu_input = Vec::new();
        for index in 0..(RATE as usize * 8) {
            let time = index as f64 / RATE as f64;
            let envelope = 1.0 + 0.4 * (TAU * 5.0 * time).sin();
            let sample = 0.3 * envelope * (TAU * hertz * time).sin();
            cpu_input.push(sample as f32);
            cpu_input.push(sample as f32);
        }
        let started = Instant::now();
        let mut cpu_output = cpu_input.clone();
        for chunk in cpu_output.chunks_mut(480 * 2) {
            effect.process(chunk, RATE);
        }
        let elapsed = started.elapsed().as_secs_f64() / 8.0;
        println!(
            "cpu {elapsed:.5} s per audio second ({:.1}x realtime), memory {} bytes, latency {} samples",
            1.0 / elapsed,
            effect.memory_bytes(),
            effect.latency_samples()
        );
        let mut bypass = tuned(0.0);
        let mut copied = cpu_input.clone();
        let started = Instant::now();
        bypass.process(&mut copied, RATE);
        println!(
            "amount 0: {:.6} s per audio second, latency {}, bit-identical {}",
            started.elapsed().as_secs_f64() / 8.0,
            bypass.latency_samples(),
            copied == cpu_input
        );
    }

    /// The grain helpers now live in `psola` and are shared with the offline
    /// shifter. Each must return exactly what the inline expression did.
    #[test]
    fn shared_grain_helpers_match_the_inline_formulas() {
        for radius in [2_usize, 3, 7, 48, 96, 218] {
            for offset in 0..(2 * radius) {
                let phase = std::f64::consts::PI * (offset as f64 + 0.5) / radius as f64;
                let inline = 0.5 - 0.5 * phase.cos();
                assert_eq!(
                    psola::grain_window(offset as f64, radius).to_bits(),
                    inline.to_bits(),
                    "window differs at radius {radius} offset {offset}"
                );
            }
        }
        for (left, centre, right) in [
            (0.2_f64, 1.0, 0.3),
            (1.0, 0.9, 0.95),
            (-2.0, 3.0, 1.0),
            (0.5, 0.5, 0.5),
            (0.0, 0.0, 0.0),
        ] {
            let denominator = left - 2.0 * centre + right;
            let inline = if denominator.abs() > 1e-12 {
                (0.5 * (left - right) / denominator).clamp(-0.5, 0.5)
            } else {
                0.0
            };
            assert_eq!(
                psola::parabolic_offset(left, centre, right).to_bits(),
                inline.to_bits()
            );
        }
        for (period, ratio) in [
            (110.0_f64, 1.059_463_094_359_295_3),
            (218.0, 0.5),
            (1.0, 1.0),
        ] {
            assert_eq!(
                psola::synth_hop(period, ratio).to_bits(),
                (period / ratio).max(2.0).to_bits()
            );
        }
        for cents in [-1_200.0_f64, -100.0, 0.0, 100.0, 1_200.0] {
            assert_eq!(
                psola::cents_ratio(cents).to_bits(),
                2f64.powf(cents / 1_200.0).to_bits()
            );
        }
        // `schedule_grains` clamps this to `(2, delay / 2)`, so the shared
        // floor of two samples is the same value the tuner used.
        for period in [0.5_f64, 2.4, 47.5, 218.2, 640.0] {
            assert_eq!(
                psola::grain_radius(period, period, 1.0),
                (period.round() as usize).max(2)
            );
        }
    }
}
