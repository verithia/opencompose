//! Granular source oscillator: a deterministic grain cloud over a decoded sample.
//!
//! The grain pool is fixed at construction and every random draw comes from a
//! seeded xorshift stream, so the same voice renders the same samples on every
//! run. Nothing in `GranularVoice::render` allocates or reads a clock.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::sampler::{DecodeChannelPolicy, decode_sample_with_policy};
pub use oc_device_schema::granular::{GrainEnvelope, PitchMode, ReadMode, SourceChannelMode};

#[path = "granular_read.rs"]
mod granular_read;

use granular_read::DecimatedSource;

/// Fixed grain pool. The pool bounds worst-case cost and is never grown.
pub const MAX_GRAINS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Window shape for each grain.
pub enum GrainWindow {
    /// Raised-cosine with flat shoulders; `window_param` widens the shoulders.
    Tukey,
    /// Half-sine attack times an exponential decay; `window_param` sets decay.
    ExpDec,
    /// Linear triangle; `window_skew` moves the peak.
    Triangle,
    /// Four-term Blackman-Harris.
    BlackmanHarris,
    /// Gaussian; `window_param` sets the width.
    Gaussian,
    /// Full raised cosine.
    Hann,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Grain-cloud settings and source sample.
pub struct GranularPatch {
    /// Absolute path to the decoded source sample.
    pub sample: PathBuf,
    /// Mono keeps the legacy source mix; stereo retains independent channels.
    #[serde(default, skip_serializing_if = "SourceChannelMode::is_mono")]
    pub channel_mode: SourceChannelMode,
    /// Choose onset-sampled or continuously followed note pitch.
    #[serde(default, skip_serializing_if = "PitchMode::is_spawn")]
    pub pitch_mode: PitchMode,
    /// Prepare low-pass levels for transposed source reads when requested.
    #[serde(default, skip_serializing_if = "ReadMode::is_linear")]
    pub read_mode: ReadMode,
    /// Override the oscillator window with a linear grain envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<GrainEnvelope>,
    /// MIDI note that plays the source at its recorded rate.
    pub root_note: f64,
    /// Grains started per second.
    pub density: f64,
    /// Base grain length in seconds.
    pub grain_length: f64,
    /// Start position in seconds.
    pub position: f64,
    /// Loop window in seconds; an empty window loops the whole sample.
    pub loop_start: f64,
    pub loop_end: f64,
    pub ping_pong: bool,
    /// Wrap the read position of each active grain within the source loop.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loop_grains: bool,
    /// Scan speed in source durations per second; zero holds the playhead.
    pub scan_rate: f64,
    pub window: GrainWindow,
    /// Shape control for [`GrainWindow`] in 0..=1.
    pub window_param: f64,
    /// Peak position in -1..=1.
    pub window_skew: f64,
    /// Position jitter as a fraction of one grain length.
    pub random_offset: f64,
    /// Length jitter as a fraction of the base grain length.
    pub random_length: f64,
    /// Pitch jitter in semitones.
    pub random_pitch: f64,
    /// Probability that one grain plays backwards.
    pub random_direction: f64,
    /// Pan jitter in -1..=1.
    pub random_pan: f64,
    /// Gain jitter as a fraction of full scale.
    pub random_gain: f64,
    pub reverse: bool,
    /// Follow the played note relative to `root_note`.
    pub pitch_track: bool,
    /// Per-voice stream seed; the importer derives it from the preset name.
    pub seed: u64,
}

impl GranularPatch {
    /// Validate physical settings before opening or decoding the source.
    pub fn validate(&self) -> Result<(), String> {
        use oc_device_schema::granular as domain;
        for (name, value, bounds) in [
            ("root_note", self.root_note, &domain::ROOT_NOTE),
            ("density", self.density, &domain::DENSITY),
            ("grain_length", self.grain_length, &domain::LENGTH),
            ("position", self.position, &domain::POSITION),
            ("loop_start", self.loop_start, &domain::POSITION),
            ("loop_end", self.loop_end, &domain::POSITION),
            ("scan_rate", self.scan_rate, &domain::SCAN_RATE),
            ("window_param", self.window_param, &domain::WINDOW_PARAM),
            ("window_skew", self.window_skew, &domain::WINDOW_SKEW),
            ("random_offset", self.random_offset, &domain::UNIT),
            ("random_length", self.random_length, &domain::UNIT),
            ("random_pitch", self.random_pitch, &domain::RANDOM_PITCH),
            ("random_direction", self.random_direction, &domain::UNIT),
            ("random_pan", self.random_pan, &domain::RANDOM_PAN),
            ("random_gain", self.random_gain, &domain::UNIT),
        ] {
            if !domain::contains(bounds, value) {
                return Err(format!(
                    "invalid granular {name}: expected finite {}..={}",
                    bounds.min, bounds.max
                ));
            }
        }
        if let Some(envelope) = self.envelope {
            envelope.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Playhead {
    seconds: f64,
    direction: f64,
}

impl Playhead {
    pub(crate) fn new(seconds: f64) -> Self {
        Self {
            seconds: if seconds.is_finite() {
                seconds.max(0.0)
            } else {
                0.0
            },
            direction: 1.0,
        }
    }

    pub(crate) fn seconds(&self) -> f64 {
        self.seconds
    }

    /// Advance by `rate` source durations per second and fold into the loop.
    pub(crate) fn advance(
        &mut self,
        rate: f64,
        duration: f64,
        loop_start: f64,
        loop_end: f64,
        ping_pong: bool,
        dt: f64,
    ) {
        if ![rate, duration, loop_start, loop_end, dt]
            .iter()
            .all(|value| value.is_finite())
            || dt < 0.0
        {
            return;
        }
        if duration <= 0.0 {
            self.seconds = 0.0;
            return;
        }
        let mut low = loop_start.clamp(0.0, duration);
        let mut high = loop_end.clamp(0.0, duration);
        if high - low < 1.0e-4 {
            low = 0.0;
            high = duration;
        }
        let next = self.seconds + rate * duration * dt * self.direction;
        if !next.is_finite() {
            return;
        }
        let offset = next - low;
        if !offset.is_finite() {
            return;
        }
        let span = high - low;
        if ping_pong {
            if (low..=high).contains(&next) {
                self.seconds = next;
                return;
            }
            let period = 2.0 * span;
            if !period.is_finite() || period <= 0.0 {
                return;
            }
            let folded = offset.rem_euclid(period);
            // Exact endpoints retain the direction of the final strict
            // reflection; landing on a boundary is not another reflection.
            let (position, direction) = if folded == 0.0 {
                (low, -1.0)
            } else if folded == span {
                (high, 1.0)
            } else if folded < span {
                (low + folded, if next > high { 1.0 } else { -1.0 })
            } else {
                (high - (folded - span), if next > high { -1.0 } else { 1.0 })
            };
            self.seconds = position;
            self.direction = direction;
        } else if span > 0.0 {
            self.seconds = low + offset.rem_euclid(span);
        }
    }
}

/// Decoded source. The spectral loader uses mono; grains may retain stereo.
pub struct SourceSample {
    pub(crate) frames: Vec<f32>,
    pub(crate) channels: usize,
    pub(crate) decimated: DecimatedSource,
    pub sample_rate: f64,
    pub duration: f64,
}

impl SourceSample {
    /// Refuse invalid settings before any source-file operation.
    pub fn load_for_patch(patch: &GranularPatch) -> Result<Self, String> {
        patch.validate()?;
        let mut sample = Self::load_with_mode(&patch.sample, patch.channel_mode)?;
        if patch.read_mode == ReadMode::Bandlimited {
            sample.decimated = DecimatedSource::prepare(&sample.frames, sample.channels)?;
        }
        Ok(sample)
    }

    /// Decode a source using the legacy mono mix, including for spectral analysis.
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_with_mode(path, SourceChannelMode::Mono)
    }

    fn load_with_mode(path: &Path, mode: SourceChannelMode) -> Result<Self, String> {
        let policy = if mode == SourceChannelMode::Stereo {
            DecodeChannelPolicy::MonoOrStereo
        } else {
            DecodeChannelPolicy::LegacyStereo
        };
        let decoded = decode_sample_with_policy(path, policy)?;
        let frame_count = decoded.frame_count();
        if frame_count == 0 {
            return Err(format!(
                "sample '{}' decoded to zero frames",
                path.display()
            ));
        }
        let input_channels = decoded.channels();
        if mode == SourceChannelMode::Stereo && input_channels > 2 {
            return Err("stereo granular sources require a mono or stereo sample".into());
        }
        let channels = if mode == SourceChannelMode::Mono {
            1
        } else {
            input_channels
        };
        let capacity = frame_count
            .checked_mul(channels)
            .ok_or("granular source size overflow")?;
        // The shared decoder bounds input samples; the grain copy never exceeds
        // that count and refuses a failed reservation instead of aborting.
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(capacity)
            .map_err(|_| "could not allocate granular source")?;
        for frame in 0..frame_count {
            if mode == SourceChannelMode::Mono {
                let mut sum = 0.0;
                for channel in 0..input_channels {
                    sum += decoded.at(frame, channel) as f64;
                }
                frames.push((sum / input_channels as f64) as f32);
            } else {
                for channel in 0..channels {
                    frames.push(decoded.at(frame, channel));
                }
            }
        }
        let sample_rate = decoded.rate().max(1) as f64;
        Ok(Self {
            duration: frame_count as f64 / sample_rate,
            decimated: DecimatedSource::default(),
            frames,
            channels,
            sample_rate,
        })
    }

    /// Number of source frames, independent of interleaved channel storage.
    pub fn frame_count(&self) -> usize {
        self.frames.len() / self.channels
    }

    /// One for a mono source mix, two for retained stereo.
    pub fn channels(&self) -> usize {
        self.channels
    }

    #[inline]
    fn read_grain(&self, position: f64, step: f64) -> (f64, f64) {
        if let Some(pair) = self
            .decimated
            .read(position, step, self.frame_count(), self.channels)
        {
            return pair;
        }
        (
            self.read_channel(position, 0),
            self.read_channel(position, 1),
        )
    }

    #[inline]
    fn read_channel(&self, position: f64, channel: usize) -> f64 {
        let frames = self.frame_count();
        if frames == 0 || !position.is_finite() {
            return 0.0;
        }
        let index = position.floor();
        let channel = channel.min(self.channels - 1);
        if index < 0.0 || index >= (frames - 1) as f64 {
            return self
                .frames
                .get(
                    (index.max(0.0) as usize)
                        .saturating_mul(self.channels)
                        .saturating_add(channel),
                )
                .copied()
                .unwrap_or(0.0) as f64;
        }
        let fraction = position - index;
        let first = self.frames[index as usize * self.channels + channel] as f64;
        let second = self.frames[(index as usize + 1) * self.channels + channel] as f64;
        first + (second - first) * fraction
    }
}

#[derive(Debug, Clone, Copy)]
struct LinearWindow {
    attack: f64,
    release: f64,
}

impl LinearWindow {
    fn at(self, progress: f64) -> f64 {
        if progress < self.attack {
            progress / self.attack
        } else if progress > 1.0 - self.release {
            (1.0 - progress) / self.release
        } else {
            1.0
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SourceLoop {
    start: f64,
    end: f64,
}

impl SourceLoop {
    fn new(source: &SourceSample, patch: &GranularPatch) -> Self {
        let count = source.frame_count() as f64;
        let frame = |seconds: f64| (seconds * source.sample_rate).round().clamp(0.0, count);
        let start = frame(patch.loop_start);
        let end = frame(patch.loop_end);
        if end > start {
            Self { start, end }
        } else {
            Self {
                start: 0.0,
                end: count,
            }
        }
    }

    #[inline]
    fn wrap(self, position: f64) -> f64 {
        if position >= self.start && position < self.end {
            return position;
        }
        let wrapped = self.start + (position - self.start).rem_euclid(self.end - self.start);
        // Rounding near the upper edge can make a positive remainder equal
        // the interval length; the exclusive end still belongs to the start.
        if wrapped >= self.end {
            self.start
        } else {
            wrapped
        }
    }

    #[inline]
    fn read(self, source: &SourceSample, position: f64, step: f64) -> (f64, f64) {
        let last = self.end - 1.0;
        if !position.is_finite() || position <= last {
            return source.read_grain(position, step);
        }
        let (left, right) = source.read_grain(last, step);
        let (start_left, start_right) = source.read_grain(self.start, step);
        let mix = position - last;
        (
            left + (start_left - left) * mix,
            right + (start_right - right) * mix,
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct Grain {
    active: bool,
    position: f64,
    step: f64,
    spawn_pitch_ratio: f64,
    read_loop: Option<SourceLoop>,
    age: f64,
    length: f64,
    gain: f64,
    pan: f64,
    window: GrainWindow,
    envelope: Option<LinearWindow>,
    window_param: f64,
    window_skew: f64,
}

impl Default for Grain {
    fn default() -> Self {
        Self {
            active: false,
            position: 0.0,
            step: 1.0,
            spawn_pitch_ratio: 1.0,
            read_loop: None,
            age: 0.0,
            length: 1.0,
            gain: 0.0,
            pan: 0.0,
            window: GrainWindow::Hann,
            envelope: None,
            window_param: 0.5,
            window_skew: 0.0,
        }
    }
}

/// Per-voice grain scheduler and pool.
#[derive(Debug, Clone)]
pub struct GranularVoice {
    grains: [Grain; MAX_GRAINS],
    rng: u64,
    spawn_debt: f64,
    time: f64,
    playhead: Playhead,
    started: bool,
}

impl GranularVoice {
    /// Create a voice with a deterministic grain seed.
    pub fn new(seed: u64) -> Self {
        Self {
            grains: [Grain::default(); MAX_GRAINS],
            rng: seed | 1,
            spawn_debt: 1.0,
            time: 0.0,
            playhead: Playhead::new(0.0),
            started: false,
        }
    }

    /// Restore the state [`GranularVoice::new`] builds without allocating.
    pub fn reset(&mut self, seed: u64) {
        self.grains = [Grain::default(); MAX_GRAINS];
        self.rng = seed | 1;
        self.spawn_debt = 1.0;
        self.time = 0.0;
        self.playhead = Playhead::new(0.0);
        self.started = false;
    }

    /// Render one stereo sample. `pitch_ratio` scales the source playback rate.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        source: &SourceSample,
        patch: &GranularPatch,
        pitch_ratio: f64,
        density_offset: f64,
        position_offset: f64,
        sample_rate: f64,
    ) -> (f64, f64) {
        // Follow mode divides by the onset ratio. Refuse invalid direct input
        // without advancing a voice whose state can be replayed deterministically.
        if patch.pitch_mode == PitchMode::Follow && (!pitch_ratio.is_finite() || pitch_ratio <= 0.0)
        {
            return (0.0, 0.0);
        }
        let mut first_grain = !self.started;
        if first_grain {
            self.playhead = Playhead::new(patch.position + position_offset);
            self.started = true;
        }
        let dt = 1.0 / sample_rate;
        self.time += dt;
        self.playhead.advance(
            patch.scan_rate,
            source.duration,
            patch.loop_start,
            patch.loop_end,
            patch.ping_pong,
            dt,
        );
        let density = (patch.density + density_offset).clamp(
            oc_device_schema::granular::DENSITY.min,
            oc_device_schema::granular::DENSITY.max,
        );
        self.spawn_debt += density * dt;
        // Bound the catch-up loop; the pool is the real limit.
        if self.spawn_debt > MAX_GRAINS as f64 {
            self.spawn_debt = MAX_GRAINS as f64;
        }
        while self.spawn_debt >= 1.0 {
            self.spawn_debt -= 1.0;
            if let Some(index) = self.grains.iter().position(|grain| !grain.active) {
                self.spawn(source, patch, pitch_ratio, sample_rate, index, first_grain);
                first_grain = false;
            }
        }
        let mut left = 0.0;
        let mut right = 0.0;
        for grain in &mut self.grains {
            if !grain.active {
                continue;
            }
            let ratio = grain.age / grain.length;
            if ratio >= 1.0 {
                grain.active = false;
                continue;
            }
            let window = match grain.envelope {
                Some(envelope) => envelope.at(ratio),
                None => grain_window(grain.window, ratio, grain.window_param, grain.window_skew),
            };
            // The unchanged-ratio path retains the exact onset arithmetic.
            // Scaling the stored step preserves each grain's direction and jitter.
            let step = if patch.pitch_mode == PitchMode::Follow
                && pitch_ratio != grain.spawn_pitch_ratio
            {
                grain.step * (pitch_ratio / grain.spawn_pitch_ratio)
            } else {
                grain.step
            };
            let (left_sample, right_sample) = match grain.read_loop {
                Some(bounds) => bounds.read(source, grain.position, step),
                None => source.read_grain(grain.position, step),
            };
            let left_value = left_sample * window * grain.gain;
            let right_value = right_sample * window * grain.gain;
            // The pan gains act independently on the channels, with no crossfeed.
            left += left_value * (1.0 - grain.pan);
            right += right_value * (1.0 + grain.pan);
            if !step.is_finite() {
                grain.active = false;
                continue;
            }
            grain.age += 1.0;
            let next = grain.position + step;
            if let Some(bounds) = grain.read_loop {
                if next.is_finite() {
                    grain.position = bounds.wrap(next);
                } else {
                    grain.active = false;
                }
            } else {
                grain.position = next;
                if grain.position < 0.0 || grain.position >= source.frame_count() as f64 {
                    grain.active = false;
                }
            }
        }
        let overlap = (density * patch.grain_length).max(1.0);
        let scale = 1.0 / overlap.sqrt();
        (left * scale, right * scale)
    }

    fn spawn(
        &mut self,
        source: &SourceSample,
        patch: &GranularPatch,
        pitch_ratio: f64,
        sample_rate: f64,
        index: usize,
        first: bool,
    ) {
        let jitter = |this: &mut Self| this.uniform();
        let offset = (jitter(self) * 2.0 - 1.0) * patch.random_offset * patch.grain_length;
        let mut direction = if patch.reverse { -1.0 } else { 1.0 };
        if jitter(self) < patch.random_direction {
            direction = -direction;
        }
        let length = patch.grain_length * (1.0 + (jitter(self) * 2.0 - 1.0) * patch.random_length);
        let length = length.clamp(
            oc_device_schema::granular::LENGTH.min,
            source.duration.max(oc_device_schema::granular::LENGTH.min),
        );
        let semitones = (jitter(self) * 2.0 - 1.0) * patch.random_pitch;
        let rate = pitch_ratio * 2f64.powf(semitones / 12.0) * direction;
        let gain = 1.0 - jitter(self) * patch.random_gain;
        let pan = (jitter(self) * 2.0 - 1.0) * patch.random_pan;
        let position = (self.playhead.seconds() + offset) * source.sample_rate;
        let read_loop =
            (patch.loop_grains && source.frame_count() > 0).then(|| SourceLoop::new(source, patch));
        self.grains[index] = Grain {
            active: true,
            position: read_loop.map_or(position, |bounds| bounds.wrap(position)),
            step: rate * source.sample_rate / sample_rate,
            spawn_pitch_ratio: pitch_ratio,
            read_loop,
            age: 0.0,
            length: (length * sample_rate).max(1.0),
            gain: gain.clamp(0.0, 1.0),
            pan: pan.clamp(-1.0, 1.0),
            window: patch.window,
            envelope: patch.envelope.map(|envelope| LinearWindow {
                attack: if first {
                    envelope.first_attack.unwrap_or(envelope.attack)
                } else {
                    envelope.attack
                },
                release: envelope.release,
            }),
            window_param: patch.window_param,
            window_skew: patch.window_skew,
        };
    }

    #[inline]
    fn uniform(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn grain_window(window: GrainWindow, ratio: f64, param: f64, skew: f64) -> f64 {
    let x = ratio.clamp(0.0, 1.0);
    let x = if skew == 0.0 {
        x
    } else {
        let exponent = 2f64.powf(-2.0 * skew.clamp(-1.0, 1.0));
        x.powf(exponent)
    };
    match window {
        GrainWindow::Hann => 0.5 - 0.5 * (std::f64::consts::TAU * x).cos(),
        GrainWindow::Tukey => {
            let shoulder = 0.05 + 0.45 * param.clamp(0.0, 1.0);
            if x < shoulder {
                0.5 - 0.5 * (std::f64::consts::PI * x / shoulder).cos()
            } else if x > 1.0 - shoulder {
                0.5 - 0.5 * (std::f64::consts::PI * (1.0 - x) / shoulder).cos()
            } else {
                1.0
            }
        }
        GrainWindow::ExpDec => {
            let decay = 1.0 + 9.0 * param.clamp(0.0, 1.0);
            (std::f64::consts::PI * x).sin() * (-decay * x).exp()
        }
        GrainWindow::Triangle => 1.0 - (2.0 * x - 1.0).abs(),
        GrainWindow::BlackmanHarris => {
            let a = std::f64::consts::TAU * x;
            0.35875 - 0.48829 * a.cos() + 0.14128 * (2.0 * a).cos() - 0.01168 * (3.0 * a).cos()
        }
        GrainWindow::Gaussian => {
            let width = 0.06 + 0.44 * param.clamp(0.0, 1.0);
            let shape = |value: f64| (-0.5 * ((value - 0.5) / width).powi(2)).exp();
            // Subtract the edge value so the truncated window ends at silence.
            let edge = shape(0.0);
            ((shape(x) - edge) / (1.0 - edge)).max(0.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(len: usize) -> SourceSample {
        let mut frames = Vec::with_capacity(len);
        for index in 0..len {
            let phase = index as f64 / 441.0;
            frames.push((phase * std::f64::consts::TAU).sin() as f32);
        }
        SourceSample {
            channels: 1,
            decimated: DecimatedSource::default(),
            frames,
            sample_rate: 44_100.0,
            duration: len as f64 / 44_100.0,
        }
    }

    fn patch() -> GranularPatch {
        GranularPatch {
            channel_mode: Default::default(),
            pitch_mode: Default::default(),
            read_mode: Default::default(),
            loop_grains: false,
            envelope: None,
            sample: PathBuf::from("fixture.wav"),
            root_note: 60.0,
            density: 40.0,
            grain_length: 0.02,
            position: 0.0,
            loop_start: 0.0,
            loop_end: 0.0,
            ping_pong: false,
            scan_rate: 0.0,
            window: GrainWindow::Hann,
            window_param: 0.5,
            window_skew: 0.0,
            random_offset: 0.0,
            random_length: 0.0,
            random_pitch: 0.0,
            random_direction: 0.0,
            random_pan: 0.0,
            random_gain: 0.0,
            reverse: false,
            pitch_track: true,
            seed: 7,
        }
    }

    fn render(patch: &GranularPatch, frames: usize) -> Vec<f64> {
        let source = source(4_410);
        let mut voice = GranularVoice::new(patch.seed);
        let mut out = Vec::with_capacity(frames);
        for _ in 0..frames {
            out.push(voice.render(&source, patch, 1.0, 0.0, 0.0, 48_000.0).0);
        }
        out
    }

    #[test]
    fn folded_playhead_matches_strict_reflections_for_signed_rates_and_endpoints() {
        for rate in [-20.0, -8.0, -1.0, 0.0, 1.0, 8.0, 20.0] {
            for start in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let mut folded = Playhead::new(start);
                let mut reference = folded;
                for _ in 0..64 {
                    reference.seconds += rate * 0.125 * reference.direction;
                    let mut steps = 0;
                    while reference.seconds > 0.75 || reference.seconds < 0.25 {
                        steps += 1;
                        assert!(steps < 64);
                        if reference.seconds > 0.75 {
                            reference.seconds = 1.5 - reference.seconds;
                            reference.direction = -1.0;
                        } else if reference.seconds < 0.25 {
                            reference.seconds = 0.5 - reference.seconds;
                            reference.direction = 1.0;
                        }
                    }
                    folded.advance(rate, 1.0, 0.25, 0.75, true, 0.125);
                    assert_eq!(
                        folded.seconds, reference.seconds,
                        "rate={rate}, start={start}"
                    );
                    assert_eq!(
                        folded.direction, reference.direction,
                        "rate={rate}, start={start}"
                    );
                }
            }
        }
    }

    #[test]
    fn extreme_playhead_travel_and_nonfinite_inputs_remain_bounded() {
        for (rate, expected, direction) in [
            (1_000_000_000_000.25, 0.25, 1.0),
            (-1_000_000_000_000.25, 0.25, 1.0),
            (1_000_000_000_000.0, 0.0, -1.0),
        ] {
            let mut playhead = Playhead::new(0.0);
            playhead.advance(rate, 1.0, 0.0, 1.0, true, 1.0);
            assert_eq!(playhead.seconds, expected);
            assert_eq!(playhead.direction, direction);
        }
        for rate in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
            let mut playhead = Playhead::new(0.25);
            playhead.advance(rate, 1.0, 0.0, 1.0, true, 2.0);
            assert_eq!(playhead.seconds, 0.25);
            assert_eq!(playhead.direction, 1.0);
        }
        let mut overflowed_offset = Playhead::new(0.25);
        overflowed_offset.advance(-1.0, f64::MAX, f64::MAX * 0.5, f64::MAX, true, 1.0);
        assert_eq!(overflowed_offset.seconds, 0.25);
        assert_eq!(Playhead::new(f64::INFINITY).seconds, 0.0);
        let mut wrapped = Playhead::new(0.0);
        wrapped.advance(-1_000_000_000_000.25, 1.0, 0.0, 1.0, false, 1.0);
        assert_eq!(wrapped.seconds, 0.75);
    }

    #[test]
    fn every_numeric_grain_field_rejects_nonfinite_values() {
        type Field = fn(&mut GranularPatch) -> &mut f64;
        let fields: [Field; 15] = [
            |p| &mut p.root_note,
            |p| &mut p.density,
            |p| &mut p.grain_length,
            |p| &mut p.position,
            |p| &mut p.loop_start,
            |p| &mut p.loop_end,
            |p| &mut p.scan_rate,
            |p| &mut p.window_param,
            |p| &mut p.window_skew,
            |p| &mut p.random_offset,
            |p| &mut p.random_length,
            |p| &mut p.random_pitch,
            |p| &mut p.random_direction,
            |p| &mut p.random_pan,
            |p| &mut p.random_gain,
        ];
        for field in fields {
            for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let mut invalid = patch();
                *field(&mut invalid) = value;
                assert!(invalid.validate().is_err());
            }
        }
        let mut valid = patch();
        valid.scan_rate = -4096.0;
        valid.random_pan = -1.0;
        assert!(
            valid.validate().is_ok(),
            "signed scans and pan jitter remain valid"
        );
    }

    #[test]
    fn invalid_grain_settings_are_refused_before_source_file_access() {
        for value in [f64::NAN, f64::INFINITY, -1.0, 0.0, 100.0] {
            let mut invalid = patch();
            invalid.grain_length = value;
            invalid.sample = PathBuf::from("missing-source-must-not-be-opened.wav");
            let error = match SourceSample::load_for_patch(&invalid) {
                Ok(_) => panic!("invalid grain length was admitted"),
                Err(error) => error,
            };
            assert!(error.contains("granular grain_length"), "{error}");
        }
        let mut valid = patch();
        valid.sample = PathBuf::from("missing-source-must-not-be-opened.wav");
        assert!(valid.validate().is_ok());
        let error = SourceSample::load_for_patch(&valid).err().unwrap();
        assert!(!error.contains("granular grain_length"), "{error}");
    }

    fn write_channels(path: &Path, channel_values: &[f32]) {
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: channel_values.len() as u16,
                sample_rate: 8_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for _ in 0..800 {
            for &value in channel_values {
                writer.write_sample(value).unwrap();
            }
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn stereo_grains_preserve_side_content_and_never_cross_channels() {
        let root = oc_test_support::temp_dir("oc-granular", "stereo");
        for values in [[0.25, -0.25], [0.25, 0.0], [0.0, 0.25]] {
            let path = root.join("source.wav");
            write_channels(&path, &values);
            let mut patch = patch();
            patch.sample = path.clone();
            patch.channel_mode = SourceChannelMode::Stereo;
            let source = SourceSample::load_for_patch(&patch).unwrap();
            assert_eq!(source.channels(), 2);
            assert_eq!(source.frame_count(), 800);
            assert_eq!(source.duration, 0.1);
            let mut voice = GranularVoice::new(patch.seed);
            let mut peak = [0.0_f64; 2];
            for _ in 0..800 {
                let (left, right) = voice.render(&source, &patch, 1.0, 0.0, 0.0, 8_000.0);
                peak[0] = peak[0].max(left.abs());
                peak[1] = peak[1].max(right.abs());
                if values[0] == -values[1] {
                    assert_eq!(left, -right);
                }
                if values[0] == 0.0 {
                    assert_eq!(left, 0.0);
                }
                if values[1] == 0.0 {
                    assert_eq!(right, 0.0);
                }
            }
            for (channel, value) in values.iter().enumerate() {
                assert_eq!(peak[channel] > 0.01, *value != 0.0);
            }
            if values[0] == -values[1] {
                assert!(
                    SourceSample::load(&path)
                        .unwrap()
                        .frames
                        .iter()
                        .all(|v| *v == 0.0)
                );
            }
        }
    }

    #[test]
    fn active_grain_wraps_at_explicit_sample_loop_and_preserves_the_seam() {
        let sample = SourceSample {
            frames: vec![0.0, 0.1, 0.2, 0.4, 0.8, 1.0],
            channels: 1,
            decimated: DecimatedSource::default(),
            sample_rate: 8_000.0,
            duration: 6.0 / 8_000.0,
        };
        let mut settings = patch();
        settings.position = 2.0 / 8_000.0;
        settings.loop_start = 2.0 / 8_000.0;
        settings.loop_end = 4.0 / 8_000.0;
        settings.density = 0.0;
        settings.envelope = Some(GrainEnvelope {
            attack: 0.0,
            release: 0.0,
            first_attack: None,
        });
        let render = |voice: &mut GranularVoice, settings: &GranularPatch, step| {
            voice.render(&sample, settings, step, 0.0, 0.0, 8_000.0).0
        };
        let val = |value| f64::from(value as f32);
        let mut voice = GranularVoice::new(settings.seed);
        assert_eq!(render(&mut voice, &settings, 1.0), val(0.2));
        assert_eq!(render(&mut voice, &settings, 1.0), val(0.4));
        assert_eq!(render(&mut voice, &settings, 1.0), val(0.8));

        settings.loop_grains = true;
        let json = serde_json::to_string(&settings).unwrap();
        assert!(
            serde_json::from_str::<GranularPatch>(&json)
                .unwrap()
                .loop_grains
        );
        voice.reset(settings.seed);
        for expected in [
            val(0.2),
            (val(0.2) + val(0.4)) / 2.0,
            val(0.4),
            (val(0.4) + val(0.2)) / 2.0,
            val(0.2),
        ] {
            assert!((render(&mut voice, &settings, 0.5) - expected).abs() < 1e-12);
        }
        settings.reverse = true;
        voice.reset(settings.seed);
        for expected in [0.2, 0.4, 0.2, 0.4] {
            assert_eq!(render(&mut voice, &settings, 101.0), val(expected));
        }
    }

    #[test]
    fn first_grain_envelope_can_start_at_full_level_then_replay_exactly() {
        let mut sample = source(32_768);
        sample.frames.fill(1.0);
        let mut settings = patch();
        settings.density = 62.5;
        settings.grain_length = 0.032;
        settings.envelope = Some(GrainEnvelope {
            attack: 0.25,
            release: 0.25,
            first_attack: Some(0.0),
        });
        let json = serde_json::to_string(&settings).unwrap();
        let reopened: GranularPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(reopened.envelope, settings.envelope);
        reopened.validate().unwrap();
        let mut voice = GranularVoice::new(reopened.seed);
        let mut output = [0.0; 256];
        let render =
            |voice: &mut GranularVoice| voice.render(&sample, &reopened, 1.0, 0.0, 0.0, 8_000.0).0;
        for sample in &mut output {
            *sample = render(&mut voice);
        }
        let at = |n: usize, expected: f64| {
            assert!((output[n] * 2.0_f64.sqrt() - expected).abs() < 1e-12);
        };
        at(0, 1.0);
        at(127, 1.0);
        at(159, 1.5);
        at(191, 2.0);
        at(224, 1.5);
        voice.reset(reopened.seed);
        for expected in output {
            assert_eq!(render(&mut voice), expected);
        }
    }

    #[test]
    fn envelope_with_invalid_phase_fractions_is_rejected() {
        let mut settings = patch();
        for (attack, release, first_attack) in [
            (0.8, 0.3, None),
            (0.1, 0.4, Some(0.7)),
            (0.1, f64::INFINITY, None),
            (0.1, 0.4, Some(f64::NAN)),
        ] {
            settings.envelope = Some(GrainEnvelope {
                attack,
                release,
                first_attack,
            });
            let error = SourceSample::load_for_patch(&settings).err().unwrap();
            assert!(error.contains("granular envelope"));
        }
    }

    #[test]
    fn bandlimited_read_mode_roundtrips_and_prepares_the_source() {
        let root = oc_test_support::temp_dir("oc-granular", "bandlimited");
        let path = root.join("source.wav");
        write_channels(&path, &[0.1, -0.3]);
        let mut settings = patch();
        settings.channel_mode = SourceChannelMode::Stereo;
        settings.read_mode = ReadMode::Bandlimited;
        settings.sample = path;
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""read_mode":"bandlimited""#));
        let reopened: GranularPatch = serde_json::from_str(&json).unwrap();
        let sample = SourceSample::load_for_patch(&reopened).unwrap();
        assert_eq!(reopened.read_mode, ReadMode::Bandlimited);
        assert!(
            sample
                .decimated
                .read(4.0, 4.0, sample.frame_count(), 2)
                .is_some()
        );
        assert_eq!(
            sample.read_grain(25.0, 1.0),
            (f64::from(0.1_f32), f64::from(-0.3_f32))
        );
    }

    #[test]
    fn mono_default_json_and_pcm_keep_the_legacy_mix_exactly() {
        let root = oc_test_support::temp_dir("oc-granular", "mono-parity");
        let path = root.join("source.wav");
        write_channels(&path, &[0.1, -0.3]);
        let mut patch = patch();
        patch.sample = path.clone();
        let json = serde_json::to_string(&patch).unwrap();
        assert!(!json.contains("channel_mode"));
        assert!(!json.contains("read_mode"));
        assert!(!json.contains("loop_grains"));
        assert!(!json.contains("envelope"));
        let roundtrip: GranularPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.channel_mode, SourceChannelMode::Mono);
        assert_eq!(roundtrip.read_mode, ReadMode::Linear);
        let actual = SourceSample::load_for_patch(&roundtrip).unwrap();
        let expected = ((0.1_f32 as f64 - 0.3_f32 as f64) / 2.0) as f32;
        let legacy = SourceSample {
            frames: vec![expected; 800],
            channels: 1,
            decimated: DecimatedSource::default(),
            sample_rate: 8_000.0,
            duration: 0.1,
        };
        assert_eq!(actual.frames, legacy.frames);
        let mut a = GranularVoice::new(patch.seed);
        let mut b = GranularVoice::new(patch.seed);
        for _ in 0..1_600 {
            assert_eq!(
                a.render(&actual, &patch, 1.0, 0.0, 0.0, 8_000.0),
                b.render(&legacy, &patch, 1.0, 0.0, 0.0, 8_000.0)
            );
        }
        patch.channel_mode = SourceChannelMode::Stereo;
        let stereo_json = serde_json::to_string(&patch).unwrap();
        assert!(stereo_json.contains("\"channel_mode\":\"stereo\""));
        assert_eq!(
            serde_json::from_str::<GranularPatch>(&stereo_json)
                .unwrap()
                .channel_mode,
            SourceChannelMode::Stereo
        );
        assert!(
            serde_json::from_str::<GranularPatch>(
                &stereo_json.replace("\"stereo\"", "\"surround\"")
            )
            .is_err()
        );
        // Stereo mode accepts mono without inventing another channel or changing its PCM.
        write_channels(&path, &[0.25]);
        patch.channel_mode = SourceChannelMode::Stereo;
        let mono = SourceSample::load_for_patch(&patch).unwrap();
        assert_eq!(mono.channels(), 1);
        assert_eq!(mono.read_channel(0.5, 0), mono.read_channel(0.5, 1));
    }

    #[test]
    fn stereo_mode_refuses_multichannel_while_mono_keeps_its_mix_policy() {
        let root = oc_test_support::temp_dir("oc-granular", "multichannel");
        let path = root.join("source.wav");
        write_channels(&path, &[0.25, -0.25, 0.5]);
        let mut patch = patch();
        patch.sample = path.clone();
        patch.channel_mode = SourceChannelMode::Stereo;
        assert!(
            SourceSample::load_for_patch(&patch)
                .err()
                .unwrap()
                .contains("mono or stereo")
        );
        let mono = SourceSample::load(&path).unwrap();
        assert_eq!(mono.channels(), 1);
        assert!(mono.frames.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn render_is_deterministic_across_two_voices() {
        let patch = patch();
        let first = render(&patch, 8_192);
        let second = render(&patch, 8_192);
        assert_eq!(first, second);
        assert!(first.iter().any(|sample| sample.abs() > 1.0e-6));
    }

    /// Note-on re-seeds an existing engine; that must equal building a new one.
    #[test]
    fn reset_restores_a_fresh_voice() {
        let patch = patch();
        let source = source(4_410);
        let mut reused = GranularVoice::new(patch.seed);
        for _ in 0..3_000 {
            reused.render(&source, &patch, 1.0, 0.0, 0.0, 48_000.0);
        }
        reused.reset(patch.seed);
        let mut fresh = GranularVoice::new(patch.seed);
        let mut reused_out = Vec::with_capacity(8_192);
        let mut fresh_out = Vec::with_capacity(8_192);
        for _ in 0..8_192 {
            reused_out.push(reused.render(&source, &patch, 1.0, 0.0, 0.0, 48_000.0));
            fresh_out.push(fresh.render(&source, &patch, 1.0, 0.0, 0.0, 48_000.0));
        }
        assert_eq!(reused_out, fresh_out);
    }

    #[test]
    fn pitch_mode_json_defaults_and_constant_pitch_pcm_are_exact() {
        let mut settings = patch();
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains("pitch_mode"));
        assert_eq!(
            serde_json::from_str::<GranularPatch>(&json)
                .unwrap()
                .pitch_mode,
            PitchMode::Spawn
        );
        settings.pitch_mode = PitchMode::Follow;
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"pitch_mode\":\"follow\""));
        assert_eq!(
            serde_json::from_str::<GranularPatch>(&json)
                .unwrap()
                .pitch_mode,
            PitchMode::Follow
        );
        assert!(
            serde_json::from_str::<GranularPatch>(&json.replace("\"follow\"", "\"unknown\""))
                .is_err()
        );
        for channels in [1, 2] {
            let mut source = source(44_100);
            if channels == 2 {
                source.frames = source
                    .frames
                    .iter()
                    .flat_map(|v| [*v, -*v * 0.25])
                    .collect();
                source.channels = 2;
            }
            settings.position = 0.5;
            settings.random_pitch = 7.0;
            settings.random_direction = 0.5;
            settings.random_pan = 0.4;
            let mut spawn_settings = settings.clone();
            spawn_settings.pitch_mode = PitchMode::Spawn;
            let mut a = GranularVoice::new(settings.seed);
            let mut b = a.clone();
            for _ in 0..8_192 {
                assert_eq!(
                    a.render(&source, &spawn_settings, 1.37, 0.0, 0.0, 48_000.0),
                    b.render(&source, &settings, 1.37, 0.0, 0.0, 48_000.0)
                );
            }
        }
    }

    #[test]
    fn following_pitch_retunes_active_grains_without_restarting_them() {
        let source = source(44_100);
        for reverse in [false, true] {
            let mut settings = patch();
            settings.pitch_mode = PitchMode::Follow;
            settings.position = 0.5;
            settings.grain_length = 0.5;
            settings.density = 0.0;
            settings.random_pitch = 7.0;
            settings.reverse = reverse;
            let mut live = GranularVoice::new(settings.seed);
            for _ in 0..64 {
                live.render(&source, &settings, 1.0, 0.0, 0.0, 48_000.0);
            }
            let before = live.grains[0];
            assert!(before.active);
            assert_eq!(before.step.is_sign_negative(), reverse);
            let rng = live.rng;
            let mut latched = live.clone();
            let mut spawn_settings = settings.clone();
            spawn_settings.pitch_mode = PitchMode::Spawn;
            // The first changed frame reads the same position and window.
            assert_eq!(
                live.render(&source, &settings, 2.0, 0.0, 0.0, 48_000.0),
                latched.render(&source, &spawn_settings, 2.0, 0.0, 0.0, 48_000.0)
            );
            let after = live.grains[0];
            assert_eq!(after.position, before.position + before.step * 2.0);
            assert_eq!(latched.grains[0].position, before.position + before.step);
            assert_eq!(after.step, before.step);
            assert_eq!(after.age, before.age + 1.0);
            assert_eq!(after.length, before.length);
            assert_eq!(after.pan, before.pan);
            assert_eq!(after.gain, before.gain);
            assert_eq!(live.rng, rng);
            live.render(&source, &settings, 0.5, 0.0, 0.0, 48_000.0);
            assert_eq!(live.grains[0].position, after.position + before.step * 0.5);
        }
    }

    #[test]
    fn invalid_follow_ratios_leave_the_voice_unchanged() {
        let source = source(44_100);
        let mut settings = patch();
        settings.pitch_mode = PitchMode::Follow;
        let mut actual = GranularVoice::new(settings.seed);
        actual.render(&source, &settings, 1.0, 0.0, 0.0, 48_000.0);
        let mut expected = actual.clone();
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                actual.render(&source, &settings, invalid, 0.0, 0.0, 48_000.0),
                (0.0, 0.0)
            );
        }
        for _ in 0..128 {
            assert_eq!(
                actual.render(&source, &settings, 1.0, 0.0, 0.0, 48_000.0),
                expected.render(&source, &settings, 1.0, 0.0, 0.0, 48_000.0)
            );
        }
    }

    #[test]
    fn density_level_follows_the_overlap_law() {
        // Below unity overlap the level grows as sqrt(density); above it the
        // 1/sqrt(overlap) normalisation holds the level flat. Random grain
        // offsets keep overlapping grains incoherent so the law is measurable.
        let rms = |density: f64| {
            let mut patch = patch();
            patch.density = density;
            patch.grain_length = 0.01;
            patch.random_offset = 0.5;
            patch.random_pitch = 0.5;
            let frames: Vec<f64> = render(&patch, 192_000);
            let tail = &frames[24_000..];
            (tail.iter().map(|sample| sample * sample).sum::<f64>() / tail.len() as f64).sqrt()
        };
        let sparse = rms(10.0);
        let denser = rms(40.0);
        let ratio = denser / sparse;
        assert!(
            (ratio - 2.0).abs() < 0.35,
            "overlap below one should give sqrt density, got {ratio}"
        );
        let flat_low = rms(100.0);
        let flat_high = rms(400.0);
        let flat_ratio = flat_high / flat_low;
        assert!(
            (flat_ratio - 1.0).abs() < 0.2,
            "overlap above one should be level-normalised, got {flat_ratio}"
        );
    }

    #[test]
    fn grain_window_is_continuous_and_ends_at_silence() {
        for window in [
            GrainWindow::Hann,
            GrainWindow::Tukey,
            GrainWindow::ExpDec,
            GrainWindow::Triangle,
            GrainWindow::BlackmanHarris,
            GrainWindow::Gaussian,
        ] {
            // Blackman-Harris leaves ~6e-5 at the edge, below -84 dB.
            assert!(grain_window(window, 0.0, 0.5, 0.0).abs() < 1.0e-4);
            assert!(grain_window(window, 1.0, 0.5, 0.0).abs() < 1.0e-4);
            assert!(grain_window(window, 0.5, 0.5, 0.0) > 0.0);
        }
    }
}
