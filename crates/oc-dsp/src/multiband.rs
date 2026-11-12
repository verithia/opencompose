//! Mode-dependent multiband compander.
//!
//! Four bands at most, split by `MultibandCrossover`. Each band has a static
//! gain law around two thresholds: down-compression above `threshold` and
//! up-compression below `threshold_below`, with `range` a bipolar clamp on the
//! gain change. A positive range clamps the change to `[-range, +range]`,
//! a negative value clamps it to `[range, 0]` (cut only), and zero is neutral.
//! Between the thresholds the band is neutral; a band with `ratio_below = 1`
//! and `threshold_below` at the floor has no up-compression.
//!
//! The detector is a stereo-linked peak follower with a fixed fast attack; its
//! release follows the band release. The user attack/release also shape the
//! gain-change envelope, where the slow stage carries sustained reduction.
//!
//! `detector = programme` adds a fast stage beside that envelope: it carries
//! whatever the user attack has not delivered yet and releases in tens of
//! milliseconds. A transient is therefore reduced at its onset, while a
//! sustained level settles on the same reduction as `detector = peak`.

use super::AudioEffect;
use super::crossover::{
    LINEAR_DELAY, LINEAR_MAX_BLOCK, MAX_BANDS, MultibandCrossover, PhaseMode, Slope,
};
use super::detector::db_to_linear;

/// Lowest value a band range control accepts, in dB.
pub const MIN_RANGE_DB: f64 = -60.0;
/// Highest value a band range control accepts, in dB.
pub const MAX_RANGE_DB: f64 = 60.0;
const MIN_THRESHOLD_DB: f64 = -60.0;
/// Highest band threshold the imported compander law reaches.
const MAX_THRESHOLD_DB: f64 = 14.2;
const MIN_RATIO: f64 = 1.0;
const MAX_RATIO: f64 = 20.0;
const MIN_ATTACK_MS: f64 = 0.1;
const MAX_ATTACK_MS: f64 = 200.0;
const MIN_RELEASE_MS: f64 = 10.0;
const MAX_RELEASE_MS: f64 = 2_000.0;
const MIN_KNEE_DB: f64 = 0.0;
const MAX_KNEE_DB: f64 = 24.0;
const MIN_MAKEUP_DB: f64 = -12.0;
const MAX_MAKEUP_DB: f64 = 24.0;
/// Longest shared lookahead line the device allocates, and its control range.
const MAX_LOOKAHEAD_MS: f64 = 20.0;

/// Share of a gain change carried by the fast ballistic stage.
const FAST_SHARE: f64 = 0.35;
/// Fast detector attack; the detector release follows the module release.
const DETECTOR_ATTACK_MS: f64 = 0.1;
/// Slow-stage release time as a multiple of the release knob.
const SLOW_RELEASE_MULTIPLIER: f64 = 3.0;
/// Attack of the fast stage in `detector = programme`. It is fast enough to
/// have delivered the reduction before a low note reaches its first peak.
const PROGRAMME_FAST_ATTACK_MS: f64 = 1.0;
/// Release of the fast stage: a transient's share is gone in tens of
/// milliseconds, so a short event never holds gain through a gap.
const PROGRAMME_FAST_RELEASE_MS: f64 = 40.0;

/// Whether a parameter id names a crossover frequency.
pub fn frequency_param(name: &str) -> bool {
    name.starts_with("crossover_")
}

/// Whether a parameter id names a per-band dB value.
pub fn db_param(name: &str) -> bool {
    matches!(
        band_field(name),
        Some((
            _,
            "threshold" | "threshold_below" | "knee" | "makeup" | "range"
        ))
    )
}

/// Whether a parameter id names a per-band time in milliseconds.
pub fn time_ms_param(name: &str) -> bool {
    matches!(
        band_field(name),
        Some((_, "attack" | "release" | "lookahead"))
    )
}

/// Whether a parameter id names a per-band ratio.
pub fn ratio_param(name: &str) -> bool {
    matches!(band_field(name), Some((_, "ratio" | "ratio_below")))
}

/// Whether a parameter id names a per-band switch.
pub fn switch_param(name: &str) -> bool {
    matches!(band_field(name), Some((_, "enabled" | "solo" | "mute")))
}

/// True for a per-band detector-lookahead control.
pub fn lookahead_param(name: &str) -> bool {
    matches!(band_field(name), Some((_, "lookahead")))
}

/// Parse a `numerator:denominator` ratio into its quotient.
pub fn ratio_code(text: &str) -> Option<f64> {
    let (numerator, denominator) = text.split_once(':')?;
    let numerator = numerator.trim().parse::<f64>().ok()?;
    let denominator = denominator.trim().parse::<f64>().ok()?;
    (numerator.is_finite() && denominator.is_finite() && numerator > 0.0 && denominator > 0.0)
        .then_some(numerator / denominator)
}

/// Map a detector name to its selector code.
pub fn detector_code(text: &str) -> Option<f64> {
    match normalize_choice(text).as_str() {
        "peak" => Some(0.0),
        "programme" | "program" => Some(1.0),
        _ => None,
    }
}

/// Map a band-count label to its selector code.
pub fn band_count_code(name: &str) -> Option<f64> {
    match normalize_choice(name).as_str() {
        "2" | "2_bands" | "two_bands" => Some(0.0),
        "3" | "3_bands" | "three_bands" => Some(1.0),
        "4" | "4_bands" | "four_bands" => Some(2.0),
        _ => None,
    }
}

/// Map a slope label to its selector code.
pub fn slope_code(name: &str) -> Option<f64> {
    match normalize_choice(name).as_str() {
        "6" | "6db" | "6_db" | "6db_oct" | "6_db_oct" | "6db_per_oct" | "6_db_per_oct" => Some(0.0),
        "12" | "12db" | "12_db" | "12db_oct" | "12_db_oct" | "12db_per_oct" | "12_db_per_oct" => {
            Some(1.0)
        }
        "24" | "24db" | "24_db" | "24db_oct" | "24_db_oct" | "24db_per_oct" | "24_db_per_oct" => {
            Some(2.0)
        }
        "36" | "36db" | "36_db" | "36db_oct" | "36_db_oct" | "36db_per_oct" | "36_db_per_oct" => {
            Some(3.0)
        }
        "48" | "48db" | "48_db" | "48db_oct" | "48_db_oct" | "48db_per_oct" | "48_db_per_oct" => {
            Some(4.0)
        }
        _ => None,
    }
}

/// Map a phase-mode label to its selector code.
pub fn phase_mode_code(name: &str) -> Option<f64> {
    match normalize_choice(name).as_str() {
        "minimum" | "minimum_phase" | "min" => Some(0.0),
        "dynamic" | "dynamic_phase" | "dyn" => Some(1.0),
        "linear" | "linear_phase" => Some(2.0),
        _ => None,
    }
}

fn normalize_choice(text: &str) -> String {
    text.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn band_field(name: &str) -> Option<(usize, &'static str)> {
    let rest = name.strip_prefix("band")?;
    let (index, field) = rest.split_once('_')?;
    let index: usize = index.parse().ok()?;
    if !(1..=MAX_BANDS).contains(&index) {
        return None;
    }
    let field = match field {
        "threshold" => "threshold",
        "threshold_below" => "threshold_below",
        "range" => "range",
        "ratio" => "ratio",
        "ratio_below" => "ratio_below",
        "attack" => "attack",
        "release" => "release",
        "knee" => "knee",
        "makeup" => "makeup",
        "enabled" => "enabled",
        "lookahead" => "lookahead",
        "solo" => "solo",
        "mute" => "mute",
        "detector" => "detector",
        _ => return None,
    };
    Some((index - 1, field))
}

/// Detector law of the whole device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectorMode {
    /// One stereo-linked peak follower with a fixed fast attack and release
    /// controlled by the band release parameter.
    Peak,
    /// A fast stage delivers the reduction the attack has not reached yet, so
    /// a transient is reduced at its onset and a sustained level settles on
    /// the same law.
    Programme,
}

/// Where one band reads its detector level from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum DetectorSource {
    /// The band's own split signal, the historical behaviour.
    #[default]
    InBand,
    /// The audio key input, so another track's level ducks this band only.
    Key,
}

impl DetectorSource {
    fn from_code(code: f64) -> Self {
        if code >= 0.5 { Self::Key } else { Self::InBand }
    }

    fn code(self) -> f64 {
        match self {
            Self::InBand => 0.0,
            Self::Key => 1.0,
        }
    }
}

impl DetectorMode {
    fn from_code(value: f64) -> Self {
        if value >= 0.5 {
            Self::Programme
        } else {
            Self::Peak
        }
    }

    fn code(self) -> f64 {
        if self == Self::Programme { 1.0 } else { 0.0 }
    }
}

#[derive(Debug, Clone, Copy)]
struct Band {
    enabled: bool,
    threshold_db: f64,
    threshold_below_db: f64,
    ratio: f64,
    ratio_below: f64,
    attack_ms: f64,
    release_ms: f64,
    knee_db: f64,
    makeup_db: f64,
    range_db: f64,
    detector_db: f64,
    fast_down: f64,
    slow_down: f64,
    fast_up: f64,
    slow_up: f64,
    detector_attack: f64,
    detector_release: f64,
    mode: DetectorMode,
    /// Fast stage of `detector = programme`.
    fast_attack: f64,
    fast_release: f64,
    attack_coefficient: f64,
    release_coefficient: f64,
    slow_release_coefficient: f64,
    last_gr_db: f64,
}

impl Band {
    fn new(sample_rate: u32) -> Self {
        let mut band = Self {
            enabled: true,
            threshold_db: 0.0,
            threshold_below_db: MIN_THRESHOLD_DB,
            ratio: 1.0,
            ratio_below: 1.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            knee_db: 6.0,
            makeup_db: 0.0,
            range_db: 0.0,
            detector_db: -96.0,
            fast_down: 0.0,
            slow_down: 0.0,
            fast_up: 0.0,
            slow_up: 0.0,
            detector_attack: 0.0,
            detector_release: 0.0,
            mode: DetectorMode::Peak,
            fast_attack: 0.0,
            fast_release: 0.0,
            attack_coefficient: 0.0,
            release_coefficient: 0.0,
            slow_release_coefficient: 0.0,
            last_gr_db: 0.0,
        };
        band.update_times(sample_rate);
        band
    }

    fn update_times(&mut self, sample_rate: u32) {
        let sr = sample_rate as f64;
        self.detector_attack = time_coefficient(DETECTOR_ATTACK_MS, sr);
        self.detector_release = time_coefficient(self.release_ms, sr);
        self.fast_attack = time_coefficient(PROGRAMME_FAST_ATTACK_MS, sr);
        self.fast_release = time_coefficient(PROGRAMME_FAST_RELEASE_MS, sr);
        self.attack_coefficient = time_coefficient(self.attack_ms, sr);
        self.release_coefficient = time_coefficient(self.release_ms, sr);
        self.slow_release_coefficient =
            time_coefficient(self.release_ms * SLOW_RELEASE_MULTIPLIER, sr);
    }

    fn reset(&mut self) {
        self.detector_db = -96.0;
        self.fast_down = 0.0;
        self.slow_down = 0.0;
        self.fast_up = 0.0;
        self.slow_up = 0.0;
        self.last_gr_db = 0.0;
    }

    fn bounds(&self) -> (f64, f64) {
        let limit = self.range_db.abs();
        if self.range_db >= 0.0 {
            (-limit, limit)
        } else {
            (-limit, 0.0)
        }
    }

    /// Static gain change in dB: down-compression above `threshold`, up
    /// compression below `threshold_below`, neutral in between.
    fn gain_law(&self, level_db: f64) -> f64 {
        if level_db < self.threshold_below_db {
            let slope = 1.0 - 1.0 / self.ratio_below;
            if slope > 0.0 {
                return slope * (self.threshold_below_db - level_db);
            }
        }
        let slope = 1.0 - 1.0 / self.ratio;
        if slope <= 0.0 {
            return 0.0;
        }
        let over = level_db - self.threshold_db;
        let knee = self.knee_db;
        if knee <= 0.0 {
            return if over > 0.0 { -slope * over } else { 0.0 };
        }
        let half = 0.5 * knee;
        if over > half {
            -slope * (over - 0.25 * knee)
        } else if over < -half {
            0.0
        } else {
            -slope * over * over.abs() / knee
        }
    }

    fn process(&mut self, left: f64, right: f64, link_db: Option<f64>) -> f64 {
        if !self.enabled {
            self.last_gr_db = 0.0;
            return 0.0;
        }
        let level_db = link_db.unwrap_or_else(|| {
            let level = left.abs().max(right.abs()).max(1e-30);
            20.0 * level.log10()
        });
        let coefficient = if level_db > self.detector_db {
            self.detector_attack
        } else {
            self.detector_release
        };
        self.detector_db = coefficient * self.detector_db + (1.0 - coefficient) * level_db;
        if self.mode == DetectorMode::Programme {
            return self.process_programme();
        }

        let target = self
            .gain_law(self.detector_db)
            .clamp(self.bounds().0, self.bounds().1);
        let down_target = (-target).max(0.0);
        let up_target = target.max(0.0);
        self.fast_down = follow(
            self.fast_down,
            down_target * FAST_SHARE,
            self.attack_coefficient,
            self.release_coefficient,
        );
        self.slow_down = follow(
            self.slow_down,
            down_target * (1.0 - FAST_SHARE),
            self.attack_coefficient,
            self.slow_release_coefficient,
        );
        // The boost is the mirror of the reduction: it builds with the release
        // time and yields with the attack time, so it never rides a transient.
        self.fast_up = follow(
            self.fast_up,
            up_target * FAST_SHARE,
            self.release_coefficient,
            self.attack_coefficient,
        );
        self.slow_up = follow(
            self.slow_up,
            up_target * (1.0 - FAST_SHARE),
            self.slow_release_coefficient,
            self.attack_coefficient,
        );
        let down = self.fast_down + self.slow_down;
        let up = self.fast_up + self.slow_up;
        let (lower, upper) = self.bounds();
        let gain_change = (up - down).clamp(lower, upper);
        self.last_gr_db = down - up;
        gain_change + self.makeup_db
    }

    /// Programme law: the sustained stage keeps the peak law and carries the
    /// sustained reduction with the user attack and release. The fast stage
    /// carries whatever the sustained stage has not delivered yet and lets go
    /// in tens of milliseconds, so a transient is reduced at its onset while a
    /// sustained level settles on exactly the peak law's reduction.
    fn process_programme(&mut self) -> f64 {
        let target = self
            .gain_law(self.detector_db)
            .clamp(self.bounds().0, self.bounds().1);
        let down_target = (-target).max(0.0);
        let up_target = target.max(0.0);
        self.slow_down = follow(
            self.slow_down,
            down_target,
            self.attack_coefficient,
            self.release_coefficient,
        );
        let undelivered = (down_target - self.slow_down).max(0.0);
        self.fast_down = follow(
            self.fast_down,
            undelivered,
            self.fast_attack,
            self.fast_release,
        );
        // The boost keeps the mirror of the peak law, so up-compression below
        // `threshold_below` behaves the same in both modes.
        self.fast_up = follow(
            self.fast_up,
            up_target * FAST_SHARE,
            self.release_coefficient,
            self.attack_coefficient,
        );
        self.slow_up = follow(
            self.slow_up,
            up_target * (1.0 - FAST_SHARE),
            self.slow_release_coefficient,
            self.attack_coefficient,
        );
        let down = self.fast_down + self.slow_down;
        let up = self.fast_up + self.slow_up;
        let (lower, upper) = self.bounds();
        let gain_change = (up - down).clamp(lower, upper);
        self.last_gr_db = -gain_change;
        gain_change + self.makeup_db
    }
}

fn follow(value: f64, target: f64, attack: f64, release: f64) -> f64 {
    let coefficient = if target > value { attack } else { release };
    coefficient * value + (1.0 - coefficient) * target
}

fn time_coefficient(ms: f64, sample_rate: f64) -> f64 {
    (-1.0 / (ms.max(0.01) * 0.001 * sample_rate)).exp()
}

/// Sample `frames` positions behind `position` in a ring written once per frame.
fn ring_at(ring: &[f64], position: usize, frames: usize) -> f64 {
    ring[(position + ring.len() - frames) % ring.len()]
}

/// A band-split compressor with per-band threshold, ratio, time and makeup.
pub struct MultibandCompressor {
    band_count: usize,
    crossover: MultibandCrossover,
    crossover_freqs: [f64; MAX_BANDS - 1],
    slope: Slope,
    phase: PhaseMode,
    bands: [Band; MAX_BANDS],
    mix: f64,
    detector_link: f64,
    detector: DetectorMode,
    sample_rate: u32,
    last_gr_db: f64,
    in_left: Vec<f64>,
    in_right: Vec<f64>,
    linear_bands: Vec<f64>,
    dry_left: Vec<f64>,
    dry_right: Vec<f64>,
    dry_pos: usize,
    lookahead_ms: f64,
    /// Rounded length of the shared audio line; also the reported latency.
    lookahead_frames: usize,
    band_lookahead_ms: [f64; MAX_BANDS],
    band_lookahead_frames: [usize; MAX_BANDS],
    band_solo: [bool; MAX_BANDS],
    band_mute: [bool; MAX_BANDS],
    band_detector: [DetectorSource; MAX_BANDS],
    /// Per-frame key level in dB while a sidechain block is processed.
    sidechain_db: Vec<f64>,
    /// Per-band audio line, written at `delay_pos` and read back by offset.
    band_delay: [[Vec<f64>; 2]; MAX_BANDS],
    /// Full-band level line for `detector_link`, sharing `delay_pos`.
    full_delay: [Vec<f64>; 2],
    delay_pos: usize,
}

impl MultibandCompressor {
    /// Prepare a two-band compressor with 1 kHz and 4 kHz crossovers.
    pub fn new(sample_rate: u32) -> Self {
        let capacity = ((sample_rate as f64 * MAX_LOOKAHEAD_MS * 0.001) as usize).max(1);
        let mut compressor = Self {
            band_count: 2,
            crossover: MultibandCrossover::new(sample_rate),
            crossover_freqs: [1_000.0, 4_000.0, 10_000.0],
            slope: Slope::Oct24,
            phase: PhaseMode::Minimum,
            bands: std::array::from_fn(|_| Band::new(sample_rate)),
            mix: 1.0,
            detector_link: 0.0,
            detector: DetectorMode::Peak,
            sample_rate,
            last_gr_db: 0.0,
            in_left: vec![0.0; LINEAR_MAX_BLOCK],
            in_right: vec![0.0; LINEAR_MAX_BLOCK],
            linear_bands: vec![0.0; MAX_BANDS * 2 * LINEAR_MAX_BLOCK],
            dry_left: vec![0.0; LINEAR_DELAY + capacity],
            dry_right: vec![0.0; LINEAR_DELAY + capacity],
            dry_pos: 0,
            lookahead_ms: 0.0,
            lookahead_frames: 0,
            band_lookahead_ms: [0.0; MAX_BANDS],
            band_lookahead_frames: [0; MAX_BANDS],
            band_solo: [false; MAX_BANDS],
            band_mute: [false; MAX_BANDS],
            band_detector: [DetectorSource::InBand; MAX_BANDS],
            sidechain_db: Vec::new(),
            band_delay: std::array::from_fn(|_| [vec![0.0; capacity + 1], vec![0.0; capacity + 1]]),
            full_delay: [vec![0.0; capacity + 1], vec![0.0; capacity + 1]],
            delay_pos: 0,
        };
        compressor.configure_crossover();
        compressor
    }

    fn configure_crossover(&mut self) {
        self.crossover.configure(
            self.band_count,
            self.crossover_freqs,
            self.slope,
            self.phase,
        );
    }

    /// Signed gain change of one band in dB; positive is attenuation.
    pub fn band_gain_reduction_db(&self, band: usize) -> f64 {
        self.bands.get(band).map_or(0.0, |band| band.last_gr_db)
    }

    /// Configured split slope.
    pub fn slope(&self) -> Slope {
        self.slope
    }

    /// Configured phase mode.
    pub fn phase(&self) -> PhaseMode {
        self.phase
    }

    fn set_band_field(&mut self, band: usize, field: &str, value: f64) {
        match field {
            "lookahead" => {
                self.band_lookahead_ms[band] = value.clamp(0.0, MAX_LOOKAHEAD_MS);
                self.refresh_band_lookahead();
                return;
            }
            "solo" => {
                self.band_solo[band] = value >= 0.5;
                return;
            }
            "mute" => {
                self.band_mute[band] = value >= 0.5;
                return;
            }
            "detector" => {
                self.band_detector[band] = DetectorSource::from_code(value);
                return;
            }
            _ => {}
        }
        let item = &mut self.bands[band];
        match field {
            "threshold" => item.threshold_db = value.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB),
            "threshold_below" => {
                item.threshold_below_db = value.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB)
            }
            "range" => item.range_db = value.clamp(MIN_RANGE_DB, MAX_RANGE_DB),
            "ratio" => item.ratio = value.clamp(MIN_RATIO, MAX_RATIO),
            "ratio_below" => item.ratio_below = value.clamp(MIN_RATIO, MAX_RATIO),
            "attack" => {
                item.attack_ms = value.clamp(MIN_ATTACK_MS, MAX_ATTACK_MS);
                item.update_times(self.sample_rate);
            }
            "release" => {
                item.release_ms = value.clamp(MIN_RELEASE_MS, MAX_RELEASE_MS);
                item.update_times(self.sample_rate);
            }
            "knee" => item.knee_db = value.clamp(MIN_KNEE_DB, MAX_KNEE_DB),
            "makeup" => item.makeup_db = value.clamp(MIN_MAKEUP_DB, MAX_MAKEUP_DB),
            "enabled" => item.enabled = value >= 0.5,
            _ => {}
        }
    }

    /// Clamp every per-band detector tap to the shared line length.
    fn refresh_band_lookahead(&mut self) {
        let sr = self.sample_rate as f64;
        for band in 0..MAX_BANDS {
            self.band_lookahead_frames[band] =
                ((sr * self.band_lookahead_ms[band] * 0.001) as usize).min(self.lookahead_frames);
        }
    }

    fn reset_delays(&mut self) {
        for band in &mut self.band_delay {
            band[0].fill(0.0);
            band[1].fill(0.0);
        }
        self.full_delay[0].fill(0.0);
        self.full_delay[1].fill(0.0);
        self.delay_pos = 0;
    }

    /// Audio and detector samples of one band. The audio is delayed by the
    /// whole line; the detector tap moves towards the live input by that band's
    /// own lookahead.
    fn delay_band(&mut self, index: usize, left: f64, right: f64) -> [f64; 4] {
        let frames = self.lookahead_frames;
        if frames == 0 {
            return [left, right, left, right];
        }
        let position = self.delay_pos;
        let detector_frames = frames - self.band_lookahead_frames[index];
        let ring = &mut self.band_delay[index];
        ring[0][position] = left;
        ring[1][position] = right;
        [
            ring_at(&ring[0], position, frames),
            ring_at(&ring[1], position, frames),
            ring_at(&ring[0], position, detector_frames),
            ring_at(&ring[1], position, detector_frames),
        ]
    }

    /// Full-band detector level at the same time position as `index`'s tap.
    fn full_level_db(&self, left: f64, right: f64, index: usize) -> f64 {
        let level = if self.lookahead_frames == 0 {
            left.abs().max(right.abs())
        } else {
            let frames = self.lookahead_frames - self.band_lookahead_frames[index];
            let position = self.delay_pos;
            ring_at(&self.full_delay[0], position, frames)
                .abs()
                .max(ring_at(&self.full_delay[1], position, frames).abs())
        };
        20.0 * level.max(1e-30).log10()
    }

    fn advance_band_delay(&mut self) {
        if self.lookahead_frames > 0 {
            self.delay_pos = (self.delay_pos + 1) % self.band_delay[0][0].len();
        }
    }

    /// True when no solo is active, so every unmuted band is audible.
    fn any_solo(&self) -> bool {
        self.band_solo[..self.band_count].iter().any(|solo| *solo)
    }

    fn audible(&self, index: usize, any_solo: bool) -> bool {
        !self.band_mute[index] && (!any_solo || self.band_solo[index])
    }

    fn process_iir(&mut self, buffer: &mut [f32]) {
        let mut frame = 0;
        while frame + 1 < buffer.len() {
            let dry_left = buffer[frame] as f64;
            let dry_right = buffer[frame + 1] as f64;
            let split = self.crossover.process_frame(dry_left, dry_right);
            if self.lookahead_frames > 0 {
                self.full_delay[0][self.delay_pos] = dry_left;
                self.full_delay[1][self.delay_pos] = dry_right;
            }
            let any_solo = self.any_solo();
            let mut wet_left = 0.0;
            let mut wet_right = 0.0;
            let mut max_gr: f64 = 0.0;
            for (index, &[split_left, split_right]) in split[..self.band_count].iter().enumerate() {
                let [audio_left, audio_right, detector_left, detector_right] =
                    self.delay_band(index, split_left, split_right);
                let level = detector_left.abs().max(detector_right.abs()).max(1e-30);
                let band_db = 20.0 * level.log10();
                let full_db = if self.detector_link > 0.0 {
                    self.full_level_db(dry_left, dry_right, index)
                } else {
                    0.0
                };
                let blended = band_db + self.detector_link * (full_db - band_db);
                let level_db = match self.band_detector[index] {
                    DetectorSource::Key => {
                        self.sidechain_db.get(frame / 2).copied().unwrap_or(blended)
                    }
                    DetectorSource::InBand => blended,
                };
                let gain_db =
                    self.bands[index].process(detector_left, detector_right, Some(level_db));
                let audible = self.audible(index, any_solo);
                if audible {
                    let gain = db_to_linear(gain_db);
                    wet_left += audio_left * gain;
                    wet_right += audio_right * gain;
                    max_gr = max_gr.max(self.bands[index].last_gr_db.max(0.0));
                }
            }
            self.last_gr_db = max_gr;
            let (dry_left, dry_right) =
                self.dry_delayed(dry_left, dry_right, self.lookahead_frames);
            let mix = self.mix;
            let output_left = if mix >= 1.0 {
                wet_left
            } else if mix <= 0.0 {
                dry_left
            } else {
                dry_left + (wet_left - dry_left) * mix
            };
            let output_right = if mix >= 1.0 {
                wet_right
            } else if mix <= 0.0 {
                dry_right
            } else {
                dry_right + (wet_right - dry_right) * mix
            };
            buffer[frame] = output_left as f32;
            buffer[frame + 1] = output_right as f32;
            self.advance_band_delay();
            frame += 2;
        }
    }

    /// Delay the dry path by `frames` samples; 0 reads the current sample.
    fn dry_delayed(&mut self, left: f64, right: f64, frames: usize) -> (f64, f64) {
        let length = self.dry_left.len();
        if frames == 0 || length == 0 {
            return (left, right);
        }
        let position = self.dry_pos;
        let read = (position + length - frames.min(length)) % length;
        let output = (self.dry_left[read], self.dry_right[read]);
        self.dry_left[position] = left;
        self.dry_right[position] = right;
        self.dry_pos = (position + 1) % length;
        output
    }

    fn process_linear(&mut self, buffer: &mut [f32], sample_rate: u32) {
        let frames = buffer.len() / 2;
        let mut offset = 0;
        while offset < frames {
            let block = (frames - offset).min(LINEAR_MAX_BLOCK);
            for frame in 0..block {
                self.in_left[frame] = buffer[(offset + frame) * 2] as f64;
                self.in_right[frame] = buffer[(offset + frame) * 2 + 1] as f64;
            }
            self.crossover.linear_split(
                &self.in_left[..block],
                &self.in_right[..block],
                &mut self.linear_bands,
                block,
            );
            let any_solo = self.any_solo();
            let mut max_gr: f64 = 0.0;
            for frame in 0..block {
                let dry_in_left = self.in_left[frame];
                let dry_in_right = self.in_right[frame];
                if self.lookahead_frames > 0 {
                    self.full_delay[0][self.delay_pos] = dry_in_left;
                    self.full_delay[1][self.delay_pos] = dry_in_right;
                }
                let mut wet_left = 0.0;
                let mut wet_right = 0.0;
                for index in 0..self.band_count {
                    let base = (index * 2) * LINEAR_MAX_BLOCK + frame;
                    let left = self.linear_bands[base];
                    let right = self.linear_bands[base + LINEAR_MAX_BLOCK];
                    let [audio_left, audio_right, detector_left, detector_right] =
                        self.delay_band(index, left, right);
                    let band_db = detector_left
                        .abs()
                        .max(detector_right.abs())
                        .max(1e-30)
                        .log10()
                        * 20.0;
                    let full_db = if self.detector_link > 0.0 {
                        self.full_level_db(dry_in_left, dry_in_right, index)
                    } else {
                        0.0
                    };
                    let blended = band_db + self.detector_link * (full_db - band_db);
                    let level_db = match self.band_detector[index] {
                        DetectorSource::Key => self
                            .sidechain_db
                            .get(offset + frame)
                            .copied()
                            .unwrap_or(blended),
                        DetectorSource::InBand => blended,
                    };
                    let gain_db =
                        self.bands[index].process(detector_left, detector_right, Some(level_db));
                    if self.audible(index, any_solo) {
                        let gain = db_to_linear(gain_db);
                        wet_left += audio_left * gain;
                        wet_right += audio_right * gain;
                        max_gr = max_gr.max(self.bands[index].last_gr_db.max(0.0));
                    }
                }
                let (dry_left, dry_right) = self.dry_delayed(
                    dry_in_left,
                    dry_in_right,
                    LINEAR_DELAY + self.lookahead_frames,
                );
                let mix = self.mix;
                let output_left = if mix >= 1.0 {
                    wet_left
                } else if mix <= 0.0 {
                    dry_left
                } else {
                    dry_left + (wet_left - dry_left) * mix
                };
                let output_right = if mix >= 1.0 {
                    wet_right
                } else if mix <= 0.0 {
                    dry_right
                } else {
                    dry_right + (wet_right - dry_right) * mix
                };
                let position = (offset + frame) * 2;
                buffer[position] = output_left as f32;
                buffer[position + 1] = output_right as f32;
                self.advance_band_delay();
            }
            self.last_gr_db = max_gr;
            offset += block;
        }
        let _ = sample_rate;
    }
}

impl AudioEffect for MultibandCompressor {
    fn supports_sidechain(&self) -> bool {
        true
    }

    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        sample_rate: u32,
        transport: Option<oc_audio::PluginTransport>,
    ) {
        self.sidechain_db.clear();
        self.sidechain_db
            .extend(sidechain.chunks_exact(2).map(|frame| {
                let level = f64::from(frame[0].abs().max(frame[1].abs())).max(1e-30);
                20.0 * level.log10()
            }));
        self.process_with_transport(buffer, sample_rate, transport);
        self.sidechain_db.clear();
    }

    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        if self.phase.is_linear() {
            self.process_linear(buffer, sample_rate);
        } else {
            self.process_iir(buffer);
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        match name {
            "band_count" => {
                let bands = 2 + value.round().clamp(0.0, 2.0) as usize;
                if bands != self.band_count {
                    self.band_count = bands;
                    self.configure_crossover();
                }
            }
            "slope" => {
                if let Some(slope) = Slope::from_code(value)
                    && slope != self.slope
                {
                    self.slope = slope;
                    self.configure_crossover();
                }
            }
            "phase_mode" => {
                if let Some(phase) = PhaseMode::from_code(value)
                    && phase != self.phase
                {
                    self.phase = phase;
                    self.configure_crossover();
                }
            }
            "crossover_1" | "crossover_2" | "crossover_3" => {
                let index = match name {
                    "crossover_1" => 0,
                    "crossover_2" => 1,
                    _ => 2,
                };
                let freq = value.clamp(20.0, 20_000.0);
                if freq != self.crossover_freqs[index] {
                    self.crossover_freqs[index] = freq;
                    self.configure_crossover();
                }
            }
            "mix" => self.mix = value.clamp(0.0, 1.0),
            "detector_link" => self.detector_link = value.clamp(0.0, 1.0),
            "detector" => {
                self.detector = DetectorMode::from_code(value);
                for band in &mut self.bands {
                    band.mode = self.detector;
                }
            }
            "lookahead" => {
                let ms = value.clamp(0.0, MAX_LOOKAHEAD_MS);
                let capacity = self.band_delay[0][0].len() - 1;
                let frames = ((self.sample_rate as f64 * ms * 0.001) as usize).min(capacity);
                self.lookahead_ms = ms;
                if frames != self.lookahead_frames {
                    self.lookahead_frames = frames;
                    self.reset_delays();
                }
                self.refresh_band_lookahead();
            }
            _ => {
                if let Some((band, field)) = band_field(name) {
                    self.set_band_field(band, field, value);
                }
            }
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "band_count" => Some((self.band_count - 2) as f64),
            "slope" => Some(self.slope.code()),
            "phase_mode" => Some(self.phase.code()),
            "crossover_1" => Some(self.crossover_freqs[0]),
            "crossover_2" => Some(self.crossover_freqs[1]),
            "crossover_3" => Some(self.crossover_freqs[2]),
            "mix" => Some(self.mix),
            "detector_link" => Some(self.detector_link),
            "detector" => Some(self.detector.code()),
            "lookahead" => Some(self.lookahead_ms),
            _ => {
                let (index, field) = band_field(name)?;
                if field == "lookahead" {
                    return Some(self.band_lookahead_ms[index]);
                }
                if field == "solo" {
                    return Some(if self.band_solo[index] { 1.0 } else { 0.0 });
                }
                if field == "mute" {
                    return Some(if self.band_mute[index] { 1.0 } else { 0.0 });
                }
                if field == "detector" {
                    return Some(self.band_detector[index].code());
                }
                let band = self.bands.get(index)?;
                match field {
                    "threshold" => Some(band.threshold_db),
                    "threshold_below" => Some(band.threshold_below_db),
                    "range" => Some(band.range_db),
                    "ratio" => Some(band.ratio),
                    "ratio_below" => Some(band.ratio_below),
                    "attack" => Some(band.attack_ms),
                    "release" => Some(band.release_ms),
                    "knee" => Some(band.knee_db),
                    "makeup" => Some(band.makeup_db),
                    "enabled" => Some(if band.enabled { 1.0 } else { 0.0 }),
                    _ => None,
                }
            }
        }
    }

    fn reset(&mut self) {
        self.crossover.reset();
        for band in &mut self.bands {
            band.reset();
        }
        self.dry_left.fill(0.0);
        self.dry_right.fill(0.0);
        self.dry_pos = 0;
        self.reset_delays();
        self.last_gr_db = 0.0;
    }

    fn latency_samples(&self) -> u32 {
        self.crossover.latency_samples() + self.lookahead_frames as u32
    }

    fn tail_samples(&self) -> u32 {
        self.crossover.latency_samples()
    }

    fn gain_reduction_db(&self) -> f64 {
        self.last_gr_db
    }

    fn name(&self) -> &str {
        "Multiband Compander"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crossover::LINEAR_TAPS;

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

    fn rms(buffer: &[f32], start_frame: usize) -> f64 {
        let samples = &buffer[start_frame * 2..];
        (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    fn compressor(bands: usize, slope: Slope, phase: PhaseMode) -> MultibandCompressor {
        let mut compressor = MultibandCompressor::new(SR);
        compressor.set_param("band_count", (bands - 2) as f64);
        compressor.set_param("slope", slope.code());
        compressor.set_param("phase_mode", phase.code());
        compressor
    }

    fn configured_band(
        compressor: &mut MultibandCompressor,
        band: usize,
        range: f64,
        threshold: f64,
        ratio: f64,
    ) {
        let prefix = format!("band{band}");
        compressor.set_param(&format!("{prefix}_threshold"), threshold);
        compressor.set_param(&format!("{prefix}_range"), range);
        compressor.set_param(&format!("{prefix}_ratio"), ratio);
        compressor.set_param(&format!("{prefix}_knee"), 0.0);
        compressor.set_param(&format!("{prefix}_attack"), 1.0);
        compressor.set_param(&format!("{prefix}_release"), 50.0);
    }

    fn measured_gain(compressor: &mut MultibandCompressor, freq: f64, amplitude: f64) -> f64 {
        let frames = 48_000;
        let input = sine(freq, amplitude, frames);
        let reference = rms(&input, 24_000);
        let mut output = input.clone();
        compressor.process(&mut output, SR);
        let latency = compressor.latency_samples() as usize;
        let measured = rms(&output, 24_000 + latency);
        20.0 * (measured / reference).log10()
    }

    #[test]
    fn neutral_setting_is_level_neutral_for_every_mode_and_slope() {
        let frequencies = [60.0, 250.0, 1_000.0, 3_000.0, 8_000.0, 14_000.0];
        for phase in PhaseMode::ALL {
            for slope in Slope::ALL {
                for bands in 2..=4 {
                    let mut compressor = compressor(bands, slope, phase);
                    for freq in frequencies {
                        compressor.reset();
                        let frames = if phase == PhaseMode::Dynamic {
                            72_000
                        } else {
                            48_000
                        };
                        let input = sine(freq, 0.2, frames);
                        let start = frames / 2;
                        let reference = rms(&input, start);
                        let mut output = input.clone();
                        compressor.process(&mut output, SR);
                        let latency = compressor.latency_samples() as usize;
                        let measured = rms(&output, start + latency);
                        let deviation = 20.0 * (measured / reference).log10();
                        assert!(
                            deviation.abs() <= 0.1,
                            "{phase:?} {slope:?} {bands} bands at {freq} Hz deviated {deviation} dB"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn positive_range_allows_boost_and_cut() {
        let mut compressor = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("crossover_1", 1_000.0);
        configured_band(&mut compressor, 1, 6.0, -20.0, 10.0);
        compressor.set_param("band1_threshold_below", -30.0);
        compressor.set_param("band1_ratio_below", 10.0);
        let cut = measured_gain(&mut compressor, 200.0, 0.9);
        println!("range +6 cut: {cut:.3} dB");
        assert!(
            (cut + 6.0).abs() < 0.4,
            "positive range cut was {cut} dB, expected -6"
        );
        compressor.reset();
        let boost = measured_gain(&mut compressor, 200.0, 0.01);
        println!("range +6 boost: {boost:.3} dB");
        assert!(
            (boost - 6.0).abs() < 0.6,
            "positive range boost was {boost} dB, expected +6"
        );
    }

    #[test]
    fn boost_does_not_ride_a_note_onset() {
        // The boost is for sustained quiet programme. A note that rises into
        // the up region must settle at the same boost as a steady tone, not
        // carry the start-of-signal transient into the attack.
        let mut compressor = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("crossover_1", 1_000.0);
        configured_band(&mut compressor, 1, 60.0, -10.0, 4.0);
        compressor.set_param("band1_threshold_below", -30.0);
        compressor.set_param("band1_ratio_below", 2.0);
        compressor.set_param("band1_attack", 90.0);
        compressor.set_param("band1_release", 250.0);

        let frames = 48_000;
        let mut onset = sine(200.0, 0.02, frames);
        for frame in 0..2_880 {
            let ramp = (frame as f32 / 2_880.0).powi(2);
            onset[frame * 2] *= ramp;
            onset[frame * 2 + 1] = onset[frame * 2];
        }
        let mut gains = Vec::new();
        for start in (0..frames).step_by(48) {
            let mut block = onset[start * 2..(start + 48) * 2].to_vec();
            compressor.process(&mut block, SR);
            gains.push(compressor.band_gain_reduction_db(0));
        }
        let settled = gains[gains.len() - 1];
        let peak = gains.iter().copied().fold(0.0f64, f64::min);
        assert!(
            peak >= settled - 1.0,
            "onset boost peaked at {peak} dB against settled {settled} dB"
        );
    }

    #[test]
    fn below_threshold_ratio_adds_a_measured_boost() {
        let mut compressor = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("crossover_1", 1_000.0);
        configured_band(&mut compressor, 1, 60.0, -10.0, 4.0);
        compressor.set_param("band1_threshold_below", -30.0);
        compressor.set_param("band1_ratio_below", 2.0);
        // A -40 dB band sits 10 dB under the up threshold; ratio 2:1 boosts 5 dB.
        let gain = measured_gain(&mut compressor, 200.0, 0.01);
        assert!((gain - 5.0).abs() < 0.5, "up-compression gain {gain} dB");
    }

    #[test]
    fn linked_detector_follows_the_full_band_level() {
        let mut compressor = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("crossover_1", 1_000.0);
        configured_band(&mut compressor, 1, 60.0, -20.0, 4.0);
        compressor.set_param("band1_attack", 1.0);
        compressor.set_param("band1_release", 50.0);
        compressor.set_param("band2_range", 0.0);
        let frames = 24_000;
        let mut buffer = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let t = frame as f64 / SR as f64;
            let value = 0.02 * (std::f64::consts::TAU * 200.0 * t).sin()
                + 0.8 * (std::f64::consts::TAU * 5_000.0 * t).sin();
            buffer[frame * 2] = value as f32;
            buffer[frame * 2 + 1] = value as f32;
        }
        let mut unlinked = buffer.clone();
        compressor.process(&mut unlinked, SR);
        let independent = compressor.band_gain_reduction_db(0);
        compressor.reset();
        compressor.set_param("detector_link", 1.0);
        let mut linked = buffer.clone();
        compressor.process(&mut linked, SR);
        let shared = compressor.band_gain_reduction_db(0);
        assert!(
            independent.abs() < 0.5,
            "unlinked low band moved {independent}"
        );
        assert!(shared > 5.0, "linked low band reduction {shared} dB");
    }

    #[test]
    fn negative_range_clamps_downward_and_blocks_boost() {
        let mut compressor = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("crossover_1", 1_000.0);
        configured_band(&mut compressor, 1, -6.0, -20.0, 10.0);
        compressor.set_param("band1_threshold_below", -30.0);
        compressor.set_param("band1_ratio_below", 10.0);
        let cut = measured_gain(&mut compressor, 200.0, 0.9);
        println!("range -6 cut: {cut:.3} dB");
        assert!(
            (cut + 6.0).abs() < 0.4,
            "negative range cut was {cut} dB, expected -6"
        );
        compressor.reset();
        let boost = measured_gain(&mut compressor, 200.0, 0.01);
        println!("range -6 boost: {boost:.3} dB");
        assert!(
            boost.abs() < 0.4,
            "negative range boosted quiet content by {boost} dB"
        );
    }

    #[test]
    fn attack_and_release_are_measured_against_the_knob() {
        let mut compressor = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("crossover_1", 1_000.0);
        configured_band(&mut compressor, 1, -12.0, -24.0, 10.0);
        compressor.set_param("band1_attack", 5.0);
        compressor.set_param("band1_release", 100.0);
        compressor.set_param("band2_range", 0.0);

        let frames = 48_000;
        let step = frames / 3;
        let mut buffer = sine(200.0, 0.7, frames);
        for frame in 0..step {
            buffer[frame * 2] = 0.0;
            buffer[frame * 2 + 1] = 0.0;
        }
        for frame in (2 * step)..frames {
            buffer[frame * 2] = 0.0;
            buffer[frame * 2 + 1] = 0.0;
        }

        let mut attack_63 = None;
        let mut release_start = None;
        let mut release_63 = None;
        for frame in 0..frames {
            let start = frame * 2;
            let mut chunk = [buffer[start], buffer[start + 1]];
            compressor.process(&mut chunk, SR);
            buffer[start] = chunk[0];
            buffer[start + 1] = chunk[1];
            if frame == 2 * step {
                release_start = Some(compressor.band_gain_reduction_db(0));
            } else if frame > 2 * step {
                let gr = compressor.band_gain_reduction_db(0);
                if release_63.is_none()
                    && let Some(start_gr) = release_start
                    && gr <= start_gr * 0.63
                {
                    release_63 = Some(frame - 2 * step);
                }
            } else if frame > step && attack_63.is_none() {
                let gr = compressor.band_gain_reduction_db(0);
                if gr >= 0.63 * 12.0 {
                    attack_63 = Some(frame - step);
                }
            }
        }
        let attack =
            attack_63.expect("gain reduction never reached 63%") as f64 / SR as f64 * 1000.0;
        println!("attack 63%: {attack:.2} ms for a 5 ms knob");
        assert!(
            (3.5..=6.5).contains(&attack),
            "attack 63% was {attack} ms for a 5 ms knob"
        );
        let release = release_63.expect("gain reduction never released to 63% of its initial value")
            as f64
            / SR as f64
            * 1000.0;
        println!("release to 63% of initial GR: {release:.2} ms for a 100 ms knob");
        assert!(
            (60.0..=150.0).contains(&release),
            "release to 63% of initial GR was {release} ms for a 100 ms knob"
        );
    }

    #[test]
    fn disabled_band_applies_no_gain_change() {
        let mut compressor = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("crossover_1", 1_000.0);
        configured_band(&mut compressor, 1, -12.0, -24.0, 10.0);
        compressor.set_param("band1_enabled", 0.0);
        let gain = measured_gain(&mut compressor, 200.0, 0.9);
        assert!(
            gain.abs() < 0.05,
            "disabled band changed level by {gain} dB"
        );
    }

    #[test]
    fn a_tone_in_band_one_only_moves_band_one() {
        for phase in [PhaseMode::Minimum, PhaseMode::Linear] {
            let mut compressor = compressor(2, Slope::Oct24, phase);
            compressor.set_param("crossover_1", 1_000.0);
            configured_band(&mut compressor, 1, -12.0, -30.0, 8.0);
            configured_band(&mut compressor, 2, -12.0, -30.0, 8.0);
            let input = sine(200.0, 0.6, 24_000);
            let mut output = input;
            compressor.process(&mut output, SR);
            let low_gr = compressor.band_gain_reduction_db(0);
            let high_gr = compressor.band_gain_reduction_db(1);
            assert!(low_gr > 5.0, "{phase:?} low band moved {low_gr} dB");
            assert!(
                high_gr < 0.5,
                "{phase:?} high band moved {high_gr} dB from a 200 Hz tone"
            );
        }
    }

    #[test]
    fn latency_is_reported_and_linear_impulse_lands_on_it() {
        for phase in PhaseMode::ALL {
            let compressor = compressor(2, Slope::Oct24, phase);
            let reported = compressor.latency_samples();
            let expected = if phase == PhaseMode::Linear {
                LINEAR_DELAY as u32
            } else {
                0
            };
            println!("latency {phase:?}: {reported} frames");
            assert_eq!(reported, expected, "{phase:?} latency");
        }

        let mut compressor = compressor(4, Slope::Oct48, PhaseMode::Linear);
        compressor.set_param("crossover_1", 200.0);
        compressor.set_param("crossover_2", 2_000.0);
        compressor.set_param("crossover_3", 8_000.0);
        let frames = LINEAR_TAPS * 2;
        let mut buffer = vec![0.0f32; frames * 2];
        buffer[0] = 0.5;
        buffer[1] = 0.5;
        compressor.process(&mut buffer, SR);
        let mut peak = 0.0;
        let mut peak_frame = 0;
        for frame in 0..frames {
            let value = buffer[frame * 2].abs();
            if value > peak {
                peak = value;
                peak_frame = frame;
            }
        }
        assert!(
            (peak_frame as i64 - LINEAR_DELAY as i64).abs() <= 2,
            "impulse peaked at {peak_frame}, expected {}",
            LINEAR_DELAY
        );
        assert!(peak > 0.49, "impulse peak was {peak}");
    }

    #[test]
    fn linear_mode_does_not_overshoot_the_declared_ceiling() {
        let mut compressor = compressor(4, Slope::Oct48, PhaseMode::Linear);
        compressor.set_param("crossover_1", 150.0);
        compressor.set_param("crossover_2", 1_500.0);
        compressor.set_param("crossover_3", 9_000.0);
        let frames = 48_000;
        let mut input = sine(100.0, 0.95, frames);
        // A step at the sine crest pushes the peak to 0.99 FS; the linear-phase
        // split must not ring above the declared 1.0 ceiling.
        for frame in 12_120..12_220 {
            let envelope = 1.0 - (frame - 12_120) as f64 / 100.0;
            let value = (input[frame * 2] as f64 + 0.04 * envelope).min(0.99) as f32;
            input[frame * 2] = value;
            input[frame * 2 + 1] = value;
        }
        let mut output = input.clone();
        compressor.process(&mut output, SR);
        let latency = compressor.latency_samples() as usize;
        let ceiling = 1.0;
        let peak = output[latency * 2..]
            .iter()
            .fold(0.0f64, |peak, sample| peak.max(sample.abs() as f64));
        println!("linear mode peak amplitude: {peak:.6}");
        assert!(
            peak <= ceiling,
            "linear mode peaked at {peak} above the {ceiling} ceiling"
        );
        // After latency compensation the output is the input, sample for sample.
        let mut worst = 0.0f64;
        for frame in 0..frames - latency {
            let expected = input[frame * 2] as f64;
            let actual = output[(frame + latency) * 2] as f64;
            worst = worst.max((actual - expected).abs());
        }
        assert!(worst < 1e-6, "linear reconstruction error was {worst}");
    }

    #[test]
    fn linear_mix_keeps_the_dry_path_latency_aligned() {
        let mut compressor = compressor(3, Slope::Oct24, PhaseMode::Linear);
        compressor.set_param("mix", 0.5);
        let frames = 24_000;
        let input = sine(500.0, 0.3, frames);
        let mut output = input.clone();
        compressor.process(&mut output, SR);
        let latency = compressor.latency_samples() as usize;
        let mut worst = 0.0f64;
        for frame in 0..frames - latency {
            let expected = input[frame * 2] as f64;
            let actual = output[(frame + latency) * 2] as f64;
            worst = worst.max((actual - expected).abs());
        }
        assert!(
            worst < 1e-6,
            "dry/wet path misalignment was {worst} after latency"
        );
    }

    #[test]
    fn reset_is_deterministic_for_every_mode() {
        for phase in PhaseMode::ALL {
            let input = sine(1_000.0, 0.3, 9_600);
            let mut compressor = compressor(3, Slope::Oct36, phase);
            configured_band(&mut compressor, 1, -9.0, -24.0, 4.0);
            let mut first = input.clone();
            compressor.process(&mut first, SR);
            compressor.reset();
            let mut second = input.clone();
            compressor.process(&mut second, SR);
            assert_eq!(first, second, "{phase:?} reset was not deterministic");
        }
    }

    #[test]
    fn extreme_settings_stay_finite() {
        for phase in PhaseMode::ALL {
            let input = sine(100.0, 1.0, 24_000);
            let mut output = input.clone();
            let mut compressor = compressor(4, Slope::Oct48, phase);
            for band in 1..=4 {
                let prefix = format!("band{band}");
                compressor.set_param(&format!("{prefix}_threshold"), -60.0);
                compressor.set_param(&format!("{prefix}_range"), 60.0);
                compressor.set_param(&format!("{prefix}_ratio"), 20.0);
                compressor.set_param(&format!("{prefix}_attack"), 0.1);
                compressor.set_param(&format!("{prefix}_release"), 10.0);
                compressor.set_param(&format!("{prefix}_knee"), 24.0);
                compressor.set_param(&format!("{prefix}_makeup"), 24.0);
                compressor.set_param(&format!("{prefix}_lookahead"), 20.0);
                compressor.set_param(&format!("{prefix}_solo"), 1.0);
                compressor.set_param(&format!("{prefix}_mute"), 1.0);
            }
            compressor.set_param("lookahead", 20.0);
            compressor.set_param("mix", 1.0);
            compressor.process(&mut output, SR);
            assert!(output.iter().all(|value| value.is_finite()));
        }
    }

    #[test]
    fn solo_and_mute_select_which_bands_sum() {
        let mut compressor = compressor(3, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("crossover_1", 500.0);
        compressor.set_param("crossover_2", 4_000.0);
        for frequency in [100.0, 1_000.0, 8_000.0] {
            let gain = measured_gain(&mut compressor, frequency, 0.2);
            println!("neutral {frequency} Hz: {gain:.2} dB");
            assert!(
                gain.abs() < 0.1,
                "{frequency} Hz was not neutral: {gain} dB"
            );
        }

        compressor.set_param("band2_solo", 1.0);
        let low = measured_gain(&mut compressor, 100.0, 0.2);
        let mid = measured_gain(&mut compressor, 1_000.0, 0.2);
        let high = measured_gain(&mut compressor, 8_000.0, 0.2);
        println!("band 2 solo: {low:.2} / {mid:.2} / {high:.2} dB");
        assert!(low < -40.0, "solo left the low band at {low} dB");
        assert!(high < -20.0, "solo left the high band at {high} dB");
        assert!(
            mid.abs() < 1.0,
            "the soloed band moved {mid} dB from its own magnitude"
        );

        compressor.reset();
        compressor.set_param("band2_solo", 0.0);
        compressor.set_param("band1_mute", 1.0);
        let low = measured_gain(&mut compressor, 100.0, 0.2);
        let mid = measured_gain(&mut compressor, 1_000.0, 0.2);
        println!("band 1 mute: {low:.2} / {mid:.2} dB");
        assert!(low < -40.0, "mute left the low band at {low} dB");
        assert!(mid.abs() < 1.0, "mute moved the mid band by {mid} dB");
    }

    #[test]
    fn mute_wins_over_solo() {
        let mut compressor = compressor(3, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("crossover_1", 500.0);
        compressor.set_param("crossover_2", 4_000.0);
        compressor.set_param("band1_solo", 1.0);
        compressor.set_param("band1_mute", 1.0);
        let low = measured_gain(&mut compressor, 100.0, 0.2);
        let mid = measured_gain(&mut compressor, 1_000.0, 0.2);
        println!("solo plus mute: {low:.2} / {mid:.2} dB");
        assert!(low < -40.0, "a muted solo still summed at {low} dB");
        assert!(mid < -20.0, "the solo did not keep the other bands out");
    }

    #[test]
    fn lookahead_is_the_reported_latency_and_delays_the_output() {
        let mut compressor = compressor(3, Slope::Oct24, PhaseMode::Linear);
        compressor.set_param("lookahead", 5.0);
        let frames = LINEAR_TAPS * 2;
        let mut buffer = vec![0.0f32; frames * 2];
        buffer[0] = 0.5;
        buffer[1] = 0.5;
        compressor.process(&mut buffer, SR);
        let expected = LINEAR_DELAY + (SR as usize * 5 / 1_000);
        assert_eq!(compressor.latency_samples() as usize, expected);
        let mut peak = 0.0;
        let mut peak_frame = 0;
        for frame in 0..frames {
            let value = buffer[frame * 2].abs();
            if value > peak {
                peak = value;
                peak_frame = frame;
            }
        }
        println!("lookahead 5 ms impulse: frame {peak_frame} of {expected}");
        assert!(
            (peak_frame as i64 - expected as i64).abs() <= 2,
            "impulse peaked at {peak_frame}, expected {expected}"
        );
        assert!(peak > 0.49, "impulse peak was {peak}");
    }

    #[test]
    fn lookahead_delays_the_dry_path_in_minimum_mode() {
        let mut compressor = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("lookahead", 5.0);
        compressor.set_param("mix", 0.0);
        let frames = 4_800;
        let mut buffer = vec![0.0f32; frames * 2];
        buffer[200 * 2] = 0.5;
        buffer[200 * 2 + 1] = 0.5;
        compressor.process(&mut buffer, SR);
        let expected = 200 + SR as usize * 5 / 1_000;
        assert_eq!(compressor.latency_samples() as usize, expected - 200);
        let mut peak = 0.0;
        let mut peak_frame = 0;
        for frame in 0..frames {
            let value = buffer[frame * 2].abs();
            if value > peak {
                peak = value;
                peak_frame = frame;
            }
        }
        println!("dry impulse: frame {peak_frame} of {expected}");
        assert_eq!(
            peak_frame, expected,
            "dry path was not delayed by lookahead"
        );
    }

    #[test]
    fn per_band_lookahead_cannot_exceed_the_shared_line() {
        let input = sine(100.0, 0.2, 9_600);
        let mut plain = input.clone();
        compressor(2, Slope::Oct24, PhaseMode::Minimum).process(&mut plain, SR);
        let mut clamped = input.clone();
        let mut compressor = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("band1_lookahead", 20.0);
        compressor.process(&mut clamped, SR);
        assert_eq!(
            plain, clamped,
            "a per-band lookahead above the line length changed the output"
        );
        assert_eq!(compressor.get_param("band1_lookahead"), Some(20.0));
        assert_eq!(compressor.latency_samples(), 0);
    }

    #[test]
    fn per_band_lookahead_anticipates_that_bands_detector() {
        // A 100 Hz burst enters band 1 ten milliseconds in. With the band's own
        // lookahead the detector reads it before the audio reaches the output,
        // so the first 5 ms of the burst are already reduced.
        let mut input = vec![0.0f32; 24_000 * 2];
        let burst_start = 2_400;
        for frame in burst_start..burst_start + 4_800 {
            let value = 0.5
                * (std::f64::consts::TAU * 100.0 * (frame - burst_start) as f64 / SR as f64).sin();
            input[frame * 2] = value as f32;
            input[frame * 2 + 1] = value as f32;
        }

        let measure = |band_lookahead: f64| {
            let mut compressor = compressor(2, Slope::Oct24, PhaseMode::Minimum);
            compressor.set_param("crossover_1", 500.0);
            configured_band(&mut compressor, 1, -12.0, -30.0, 4.0);
            compressor.set_param("band1_attack", 10.0);
            compressor.set_param("lookahead", 5.0);
            compressor.set_param("band1_lookahead", band_lookahead);
            let mut output = input.clone();
            compressor.process(&mut output, SR);
            let start = burst_start + compressor.latency_samples() as usize;
            let end = start + SR as usize * 5 / 1_000;
            let window = &output[start * 2..end * 2];
            let sum: f64 = window.iter().map(|s| (*s as f64).powi(2)).sum();
            (sum / window.len() as f64).sqrt()
        };

        let immediate = measure(0.0);
        let advanced = measure(5.0);
        let difference = 20.0 * (advanced / immediate).log10();
        println!("first 5 ms of the burst: {immediate:.5} vs {advanced:.5} ({difference:.2} dB)");
        assert!(
            difference < -1.2,
            "per-band lookahead changed the attack by only {difference} dB"
        );
    }

    #[test]
    fn defaults_render_the_pre_lookahead_digest() {
        // Pinned from the device before per-band lookahead and solo/mute
        // existed: every default path must stay bit-identical.
        let frames = 24_000;
        let mut input = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let t = n as f64 / SR as f64;
            let left = 0.55 * (std::f64::consts::TAU * 110.0 * t).sin()
                + 0.25 * (std::f64::consts::TAU * 1_000.0 * t).sin()
                + 0.15 * (std::f64::consts::TAU * 7_000.0 * t + 0.4).sin();
            let right = 0.45 * (std::f64::consts::TAU * 220.0 * t + 0.9).sin()
                + 0.2 * (std::f64::consts::TAU * 3_000.0 * t).sin();
            input.push(left as f32);
            input.push(right as f32);
        }
        let input_hash = fnv1a(&input);
        assert_eq!(input_hash, 0xa744_179c_c4e3_4298, "multiband golden input");
        let mut actual_hashes = Vec::new();
        let mut expected_hashes = Vec::new();
        for (phase, mix, expected) in [
            (0.0, 1.0, 0x09ce_a6a1_7ea4_e4da_u64),
            (0.0, 0.4, 0x3c68_e3d6_0d96_453c),
            (1.0, 0.6, 0xb564_709f_55e4_4390),
            (
                2.0,
                1.0,
                crate::golden_references::select(
                    0x1581_1173_f78c_3d7d,
                    0xe21b_a3b0_4eb4_d085,
                    0x887b_c703_c73f_d09b,
                ),
            ),
        ] {
            let mut compressor = compressor(3, Slope::Oct24, PhaseMode::Minimum);
            compressor.set_param("phase_mode", phase);
            compressor.set_param("mix", mix);
            let mut output = input.clone();
            compressor.process(&mut output, SR);
            let actual = fnv1a(&output);
            let (power, max_abs) =
                output
                    .iter()
                    .fold((0.0_f64, 0.0_f64), |(power, max_abs), &sample| {
                        let value = f64::from(sample);
                        (power + value * value, max_abs.max(value.abs()))
                    });
            let rms = (power / output.len() as f64).sqrt();
            let nonfinite = output.iter().filter(|sample| !sample.is_finite()).count();
            eprintln!(
                "multiband golden phase={phase} mix={mix} samples={} input_hash={input_hash:016x} output_hash={actual:016x} expected={expected:016x} output_rms={rms:.12e} output_max_abs={max_abs:.12e} nonfinite={nonfinite}",
                output.len()
            );
            actual_hashes.push(actual);
            expected_hashes.push(expected);
        }
        assert_eq!(
            actual_hashes, expected_hashes,
            "all multiband golden cases in declared order"
        );
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

    // ----------------------------------------------------------------------
    // Law diagnosis harness. Ignored so the suite stays fast; run with
    // `cargo test -p oc-dsp --lib -- multiband::tests::diagnose --ignored
    //  --nocapture` (single test) or `-- --ignored --nocapture` for all.
    // It isolates one band from a preset and compares it with the static
    // `gain_law` math on the same band signal, so a difference can only come
    // from the detector, the clamp or the makeup placement.
    // ----------------------------------------------------------------------

    /// The low band of `low-end-grip`: the preset that showed the largest gap.
    /// `mode` selects the detector law through the (opt-in) `detector` control.
    fn low_end_grip(mode: f64) -> MultibandCompressor {
        let mut compressor = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        compressor.set_param("crossover_1", 120.0);
        compressor.set_param("detector", mode);
        for (field, value) in [
            ("threshold", -26.0),
            ("range", -6.0),
            ("ratio", 3.0),
            ("attack", 15.0),
            ("release", 220.0),
            ("knee", 6.0),
            ("makeup", 1.0),
        ] {
            compressor.set_param(&format!("band1_{field}"), value);
        }
        // Band 2 is muted so the measured output is band 1 alone; the mute does
        // not touch band 1's detector or gain law.
        compressor.set_param("band2_mute", 1.0);
        compressor
    }

    #[derive(Clone, Copy, PartialEq)]
    enum StaticLevel {
        /// The law sees the instantaneous sample peak, no ballistics at all.
        Instant,
        /// The law sees a 10 ms sliding RMS, the textbook static compander.
        Rms10ms,
    }

    impl StaticLevel {
        fn label(self) -> &'static str {
            match self {
                Self::Instant => "static/instant",
                Self::Rms10ms => "static/rms10",
            }
        }
    }

    #[derive(Clone, Copy)]
    struct Reading {
        lufs: f64,
        crest_db: f64,
        rms_db: f64,
    }

    impl Reading {
        /// Loudness relative to RMS: the part a level match cannot remove, and
        /// the analogue of the harness's matched ΔLUFS.
        fn lufs_relative(&self) -> f64 {
            self.lufs - (self.rms_db - 0.691)
        }
    }

    fn read(mono: &[f64]) -> Reading {
        let mut weight = crate::kweight::KWeightChannel::new(SR as f64);
        let mut energy = 0.0;
        let mut peak = 0.0f64;
        for &sample in mono {
            let weighted = weight.process(sample);
            energy += weighted * weighted;
            peak = peak.max(sample.abs());
        }
        let frames = mono.len().max(1) as f64;
        let power = energy / frames;
        let lufs = if power <= 1e-12 {
            f64::NEG_INFINITY
        } else {
            -0.691 + 10.0 * power.log10()
        };
        let rms = (mono.iter().map(|sample| sample * sample).sum::<f64>() / frames).sqrt();
        Reading {
            lufs,
            crest_db: 20.0 * (peak / rms.max(1e-30)).log10(),
            rms_db: 20.0 * rms.max(1e-30).log10(),
        }
    }

    /// Band 1 of the same crossover the device uses, so both sides see one signal.
    fn band_one(input: &[f32]) -> Vec<f64> {
        let mut crossover = MultibandCrossover::new(SR);
        let mut freqs = [120.0; MAX_BANDS - 1];
        freqs[1] = 4_000.0;
        freqs[2] = 10_000.0;
        crossover.configure(2, freqs, Slope::Oct24, PhaseMode::Minimum);
        let frames = input.len() / 2;
        let mut band = Vec::with_capacity(frames);
        for frame in 0..frames {
            let split =
                crossover.process_frame(input[frame * 2] as f64, input[frame * 2 + 1] as f64);
            band.push(split[0][0]);
        }
        band
    }

    fn run_device(compressor: &mut MultibandCompressor, input: &[f32]) -> (Vec<f64>, Vec<f64>) {
        let frames = input.len() / 2;
        let mut output = Vec::with_capacity(frames);
        let mut reduction = Vec::with_capacity(frames);
        for frame in 0..frames {
            let mut block = [input[frame * 2], input[frame * 2 + 1]];
            compressor.process(&mut block, SR);
            output.push(block[0] as f64);
            reduction.push(compressor.band_gain_reduction_db(0));
        }
        (output, reduction)
    }

    fn run_static(
        band: &[f64],
        compressor: &MultibandCompressor,
        level: StaticLevel,
    ) -> (Vec<f64>, Vec<f64>) {
        let definition = &compressor.bands[0];
        let (lower, upper) = definition.bounds();
        let window = (SR as f64 * 0.010) as usize;
        let mut squares = std::collections::VecDeque::with_capacity(window + 1);
        let mut sum = 0.0;
        let mut output = Vec::with_capacity(band.len());
        let mut reduction = Vec::with_capacity(band.len());
        for &sample in band {
            let level_db = match level {
                StaticLevel::Instant => 20.0 * sample.abs().max(1e-30).log10(),
                StaticLevel::Rms10ms => {
                    let square = sample * sample;
                    squares.push_back(square);
                    sum += square;
                    if squares.len() > window {
                        sum -= squares.pop_front().unwrap_or(0.0);
                    }
                    let mean = sum / squares.len().max(1) as f64;
                    10.0 * mean.max(1e-30).log10()
                }
            };
            let change = definition.gain_law(level_db).clamp(lower, upper);
            // `band_gain_reduction_db` excludes makeup, so the static side does
            // too and the two traces are directly comparable.
            reduction.push(-change);
            output.push(sample * 10f64.powf((change + definition.makeup_db) / 20.0));
        }
        (output, reduction)
    }

    fn mean(values: &[f64]) -> f64 {
        if values.is_empty() {
            0.0
        } else {
            values.iter().sum::<f64>() / values.len() as f64
        }
    }

    fn peak(values: &[f64]) -> f64 {
        values.iter().copied().fold(0.0f64, f64::max)
    }

    /// One 60 Hz burst train: 40 ms on, 160 ms off, ten bursts.
    fn burst_programme() -> Vec<f32> {
        let frames = SR as usize * 2;
        let mut input = vec![0.0f32; frames * 2];
        let period = SR as usize / 5;
        let on = SR as usize * 40 / 1_000;
        for burst in 0..10 {
            let start = burst * period + SR as usize / 20;
            for frame in start..(start + on).min(frames) {
                let value =
                    0.5 * (std::f64::consts::TAU * 60.0 * (frame - start) as f64 / SR as f64).sin();
                input[frame * 2] = value as f32;
                input[frame * 2 + 1] = value as f32;
            }
        }
        input
    }

    /// A quiet 55 Hz bed with eight loud 30 ms hits every 250 ms: a transient
    /// over a steady programme, which is what a band compressor meets in a mix.
    fn transient_steady_programme() -> Vec<f32> {
        let frames = SR as usize * 2;
        let mut input = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let bed = 0.06
                * (std::f64::consts::TAU * 55.0 * frame as f64 / SR as f64
                    + 0.3 * (std::f64::consts::TAU * 0.7 * frame as f64 / SR as f64).sin())
                .sin();
            input[frame * 2] = bed as f32;
            input[frame * 2 + 1] = bed as f32;
        }
        for hit in 0..8 {
            let start = SR as usize / 4 + hit * (SR as usize / 4);
            for frame in start..(start + SR as usize * 30 / 1_000).min(frames) {
                let value = 0.55
                    * (std::f64::consts::TAU * 55.0 * (frame - start) as f64 / SR as f64).sin();
                input[frame * 2] = value as f32;
                input[frame * 2 + 1] = value as f32;
            }
        }
        input
    }

    fn report_row(label: &str, output: &[f64], reduction: &[f64]) -> String {
        let reading = read(output);
        format!(
            "  {label:<22} LUFS {:>8.2}  LU/RMS {:>6.2}  crest {:>6.2} dB  rms {:>8.2} dB  \
             GR mean {:>6.2} max {:>6.2} dB",
            reading.lufs,
            reading.lufs_relative(),
            reading.crest_db,
            reading.rms_db,
            mean(reduction),
            peak(reduction)
        )
    }

    fn print_programme(label: &str, input: &[f32], mode: f64) {
        let band = band_one(input);
        println!("\n{label}: detector mode {mode}");
        let input_reading = read(&band);
        println!(
            "  {:<22} LUFS {:>8.2}  LU/RMS {:>6.2}  crest {:>6.2} dB  rms {:>8.2} dB",
            "input band 1",
            input_reading.lufs,
            input_reading.lufs_relative(),
            input_reading.crest_db,
            input_reading.rms_db
        );
        let mut device = low_end_grip(mode);
        let (device_output, device_reduction) = run_device(&mut device, input);
        println!(
            "{}",
            report_row("device", &device_output, &device_reduction)
        );
        for level in [StaticLevel::Instant, StaticLevel::Rms10ms] {
            let (static_output, static_reduction) = run_static(&band, &low_end_grip(mode), level);
            println!(
                "{}",
                report_row(level.label(), &static_output, &static_reduction)
            );
            let difference: Vec<f64> = device_reduction
                .iter()
                .zip(&static_reduction)
                .map(|(device, static_gr)| (device - static_gr).abs())
                .collect();
            println!(
                "    mean |device GR - {}| = {:.2} dB",
                level.label(),
                mean(&difference)
            );
        }
        // Makeup: it is added after the clamp in every band, so with three or
        // four bands the preset's makeups sum in the output.
        let mut no_makeup = low_end_grip(mode);
        no_makeup.set_param("band1_makeup", 0.0);
        let (plain, plain_reduction) = run_device(&mut no_makeup, input);
        println!(
            "{}",
            report_row("device, makeup 0", &plain, &plain_reduction)
        );
        println!(
            "    makeup delta: {:.2} dB of level, {:.2} dB of crest",
            read(&device_output).lufs - read(&plain).lufs,
            read(&device_output).crest_db - read(&plain).crest_db
        );
        // Clamp: opening the range to +-60 dB removes the cut-only ceiling.
        let mut open = low_end_grip(mode);
        open.set_param("band1_range", 60.0);
        let (open_output, open_reduction) = run_device(&mut open, input);
        println!(
            "{}",
            report_row("device, range +60", &open_output, &open_reduction)
        );
        println!(
            "    clamp delta: mean GR {:.2} dB, max GR {:.2} dB",
            mean(&open_reduction) - mean(&device_reduction),
            peak(&open_reduction) - peak(&device_reduction)
        );
    }

    /// Gain reduction and output level around the first burst: how much of a
    /// transient the device actually attenuates, against the static law.
    fn active_input() -> Vec<f32> {
        let frames = 24_000;
        let mut input = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let t = n as f64 / SR as f64;
            let burst = if (n / 2_400) % 2 == 0 { 1.0 } else { 0.25 };
            let left = burst
                * (0.55 * (std::f64::consts::TAU * 55.0 * t).sin()
                    + 0.25 * (std::f64::consts::TAU * 900.0 * t).sin()
                    + 0.15 * (std::f64::consts::TAU * 6_000.0 * t + 0.4).sin());
            let right = burst
                * (0.45 * (std::f64::consts::TAU * 110.0 * t + 0.9).sin()
                    + 0.2 * (std::f64::consts::TAU * 2_500.0 * t).sin());
            input.push(left as f32);
            input.push(right as f32);
        }
        input
    }

    /// A device with one active band, built without touching `detector`, so the
    /// digest pins the arithmetic the device had before the control existed.
    fn active_low_band() -> MultibandCompressor {
        let mut device = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        device.set_param("crossover_1", 120.0);
        for (field, value) in [
            ("threshold", -26.0),
            ("range", -6.0),
            ("ratio", 3.0),
            ("attack", 15.0),
            ("release", 220.0),
            ("knee", 6.0),
            ("makeup", 1.0),
        ] {
            device.set_param(&format!("band1_{field}"), value);
        }
        device.set_param("band2_mute", 1.0);
        device
    }

    fn active_digest(device: &mut MultibandCompressor) -> u64 {
        let mut output = active_input();
        device.process(&mut output, SR);
        fnv1a(&output)
    }

    #[test]
    fn detector_mode_is_off_by_default_and_keeps_the_pinned_digest() {
        // Pinned from the render before the `detector` control existed. An
        // active band, so the digest covers the gain law, not just the sum.
        const PINNED: u64 = 0x9821_4045_5a67_e314;
        let mut untouched = active_low_band();
        assert_eq!(untouched.get_param("detector"), Some(0.0));
        assert_eq!(active_digest(&mut untouched), PINNED);

        let mut explicit = active_low_band();
        explicit.set_param("detector", 0.0);
        assert_eq!(active_digest(&mut explicit), PINNED);

        let mut programme = active_low_band();
        programme.set_param("detector", 1.0);
        assert_ne!(
            active_digest(&mut programme),
            PINNED,
            "the programme mode must change the render"
        );
        assert_eq!(programme.get_param("detector"), Some(1.0));
    }

    #[test]
    fn programme_mode_reduces_crest_on_a_transient_programme() {
        let input = transient_steady_programme();
        let crest = |mode: f64| {
            let mut device = low_end_grip(mode);
            let (output, _) = run_device(&mut device, &input);
            read(&output).crest_db
        };
        let peak = crest(0.0);
        let programme = crest(1.0);
        let margin = peak - programme;
        println!(
            "crest: peak mode {peak:.2} dB, programme mode {programme:.2} dB ({margin:.2} dB down)"
        );
        assert!(
            margin >= 2.0,
            "programme mode reduced the crest by only {margin:.2} dB"
        );
    }

    #[test]
    fn programme_mode_matches_the_peak_law_on_a_steady_tone() {
        // A sustained level must settle on the same reduction in both modes;
        // only the transient response may differ.
        for amplitude_db in [-30.0, -26.0, -22.0, -18.0, -14.0, -10.0] {
            let amplitude = 10f64.powf(amplitude_db / 20.0);
            let input = sine(60.0, amplitude, 24_000);
            let settled = |mode: f64| {
                let (_, reduction) = run_device(&mut low_end_grip(mode), &input);
                reduction[reduction.len() - 1]
            };
            let peak = settled(0.0);
            let programme = settled(1.0);
            println!("{amplitude_db} dBFS: peak {peak:.3} dB, programme {programme:.3} dB");
            assert!(
                (peak - programme).abs() <= 0.2,
                "{amplitude_db} dBFS settled at {peak:.3} vs {programme:.3} dB"
            );
        }
    }

    #[test]
    fn programme_mode_still_boosts_below_threshold_below() {
        let mut device = compressor(2, Slope::Oct24, PhaseMode::Minimum);
        device.set_param("crossover_1", 1_000.0);
        configured_band(&mut device, 1, 60.0, -10.0, 4.0);
        device.set_param("band1_threshold_below", -30.0);
        device.set_param("band1_ratio_below", 2.0);
        device.set_param("detector", 1.0);
        let gain = measured_gain(&mut device, 200.0, 0.01);
        println!("programme mode up-compression gain: {gain:.2} dB");
        assert!((gain - 5.0).abs() < 0.6, "up-compression gain {gain} dB");
    }

    #[test]
    fn programme_mode_respects_the_range_clamp() {
        let input = transient_steady_programme();
        for range in [-3.0, 4.0] {
            let mut device = low_end_grip(0.0);
            device.set_param("band1_range", range);
            device.set_param("detector", 1.0);
            let (_, reduction) = run_device(&mut device, &input);
            let worst = reduction.iter().copied().fold(0.0f64, f64::max);
            let worst = (-reduction.iter().copied().fold(0.0f64, f64::min)).max(worst);
            println!("range {range}: worst |GR| {worst:.2} dB");
            assert!(
                worst <= range.abs() + 1e-6,
                "range {range} let the reduction reach {worst} dB"
            );
        }
    }

    #[test]
    #[ignore = "measurement harness; run with --ignored --nocapture"]
    fn diagnose_burst_onset() {
        let input = burst_programme();
        let band = band_one(&input);
        let static_definition = low_end_grip(0.0);
        for mode in [0.0, 1.0] {
            let mut device = low_end_grip(mode);
            let (output, reduction) = run_device(&mut device, &input);
            let (static_output, static_reduction) =
                run_static(&band, &static_definition, StaticLevel::Rms10ms);
            let onset = SR as usize / 20;
            // 0.5, 2, 5, 10, 20 and 39 ms into the 40 ms burst, then 60 ms
            // into the gap that follows it.
            let marks = [24usize, 96, 240, 480, 960, 1_872, 2_880];
            let mut line = format!("  detector mode {mode}:");
            for mark in marks {
                line.push_str(&format!(
                    "\n    +{:<5.1} ms  device GR {:>5.2}  static GR {:>5.2}  output {:>7.2} vs {:>7.2} dBFS",
                    mark as f64 * 1_000.0 / SR as f64,
                    reduction[onset + mark],
                    static_reduction[onset + mark],
                    20.0 * output[onset + mark].abs().max(1e-30).log10(),
                    20.0 * static_output[onset + mark].abs().max(1e-30).log10()
                ));
            }
            println!("{line}");
        }
    }

    #[test]
    #[ignore = "measurement harness; run with --ignored --nocapture"]
    fn diagnose_programme_law() {
        for mode in [0.0, 1.0] {
            print_programme("tone bursts 60 Hz", &burst_programme(), mode);
            print_programme(
                "transient + steady programme",
                &transient_steady_programme(),
                mode,
            );
        }

        // Transfer curve on a steady tone: the law without ballistics.
        println!("\nsteady 60 Hz transfer curve, device vs static law");
        let mut device = low_end_grip(0.0);
        let definition = low_end_grip(0.0);
        for step in 0..11 {
            let amplitude = 10f64.powf((-50.0 + 4.0 * step as f64) / 20.0);
            let input = sine(60.0, amplitude, 24_000);
            let (output, reduction) = run_device(&mut device, &input);
            let band = band_one(&input);
            let (static_output, static_reduction) =
                run_static(&band, &definition, StaticLevel::Instant);
            let settled = reduction.len() - 1;
            let static_settled = static_reduction.len() - 1;
            println!(
                "  {:>6.1} dBFS: device GR {:>6.2} dB  static {:>6.2} dB  \
                 output {:>7.2} vs {:>7.2} dB  crest {:>5.2} dB",
                -50.0 + 4.0 * step as f64,
                reduction[settled],
                static_reduction[static_settled],
                read(&output).rms_db,
                read(&static_output).rms_db,
                read(&output).crest_db
            );
        }
    }

    fn tone_amplitude(samples: &[f32], rate: u32, hz: f64) -> f64 {
        let mut re = 0.0;
        let mut im = 0.0;
        for (index, sample) in samples.iter().enumerate() {
            let phase = 2.0 * std::f64::consts::PI * hz * index as f64 / f64::from(rate);
            re += f64::from(*sample) * phase.cos();
            im += f64::from(*sample) * phase.sin();
        }
        let len = samples.len().max(1) as f64;
        2.0 * (re * re + im * im).sqrt() / len
    }

    /// A keyed band follows the sidechain while its neighbours stay in band.
    #[test]
    fn an_external_key_ducks_only_its_own_band() {
        let rate = 48_000u32;
        let frames = 24_000;
        let mut signal = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let time = frame as f64 / f64::from(rate);
            let low = (2.0 * std::f64::consts::PI * 100.0 * time).sin() * 0.3;
            let high = (2.0 * std::f64::consts::PI * 5_000.0 * time).sin() * 0.3;
            let value = (low + high) as f32;
            signal[frame * 2] = value;
            signal[frame * 2 + 1] = value;
        }
        let key = |loud: bool| -> Vec<f32> {
            let mut buffer = vec![0.0f32; frames * 2];
            if loud {
                for frame in frames / 2..frames {
                    let time = frame as f64 / f64::from(rate);
                    let value = (2.0 * std::f64::consts::PI * 1_000.0 * time).sin() * 0.9;
                    buffer[frame * 2] = value as f32;
                    buffer[frame * 2 + 1] = value as f32;
                }
            }
            buffer
        };
        let build = || {
            let mut compressor = MultibandCompressor::new(rate);
            compressor.set_param("band_count", 1.0);
            compressor.set_param("crossover_1", 300.0);
            compressor.set_param("crossover_2", 3_000.0);
            compressor.set_param("mix", 1.0);
            compressor.set_param("band1_detector", 1.0);
            compressor.set_param("band1_threshold", -40.0);
            compressor.set_param("band1_ratio", 8.0);
            compressor.set_param("band1_range", -24.0);
            compressor.set_param("band1_attack", 1.0);
            compressor.set_param("band1_release", 60.0);
            compressor
        };
        let run = |loud_key: bool| {
            let mut compressor = build();
            let mut buffer = signal.clone();
            compressor.process_with_sidechain(&mut buffer, &key(loud_key), rate, None);
            let left: Vec<f32> = buffer.chunks_exact(2).map(|frame| frame[0]).collect();
            let second_half = &left[frames / 2..];
            (
                tone_amplitude(&{ second_half.to_vec() }, rate, 100.0),
                tone_amplitude(&{ second_half.to_vec() }, rate, 5_000.0),
            )
        };
        let (low_quiet, high_quiet) = run(false);
        let (low_loud, high_loud) = run(true);
        assert!(
            low_loud < low_quiet * 0.4,
            "the keyed band barely ducked: {low_loud} vs {low_quiet}"
        );
        assert!(
            (high_loud / high_quiet - 1.0).abs() < 0.1,
            "the neighbour band moved: {high_loud} vs {high_quiet}"
        );
    }
}
