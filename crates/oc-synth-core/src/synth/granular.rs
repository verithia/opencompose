//! Granular source oscillator: a deterministic grain cloud over a decoded sample.
//!
//! The grain pool is fixed at construction and every random draw comes from a
//! seeded xorshift stream, so the same voice renders the same samples on every
//! run. Nothing in `GranularVoice::render` allocates or reads a clock.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::sampler::decode_sample;

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

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Playhead {
    seconds: f64,
    direction: f64,
}

impl Playhead {
    pub(crate) fn new(seconds: f64) -> Self {
        Self {
            seconds: seconds.max(0.0),
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
        self.seconds += rate * duration * dt * self.direction;
        if ping_pong {
            while self.seconds > high || self.seconds < low {
                if self.seconds > high {
                    self.seconds = 2.0 * high - self.seconds;
                    self.direction = -1.0;
                } else if self.seconds < low {
                    self.seconds = 2.0 * low - self.seconds;
                    self.direction = 1.0;
                }
            }
        } else {
            let span = high - low;
            if span > 0.0 {
                self.seconds = low + (self.seconds - low).rem_euclid(span);
            }
        }
    }
}

/// Decoded, mono source used by the grain and spectral engines.
pub struct SourceSample {
    pub(crate) frames: Vec<f32>,
    pub sample_rate: f64,
    pub duration: f64,
}

impl SourceSample {
    /// Decode a sample file into the granular source.
    pub fn load(path: &Path) -> Result<Self, String> {
        let decoded = decode_sample(path)?;
        let channels = decoded.frame_count().max(1);
        let mut frames = Vec::with_capacity(channels);
        let count = decoded.channels();
        for frame in 0..channels {
            let mut sum = 0.0;
            for channel in 0..count {
                sum += decoded.at(frame, channel) as f64;
            }
            frames.push((sum / count as f64) as f32);
        }
        if frames.is_empty() {
            return Err(format!(
                "sample '{}' decoded to zero frames",
                path.display()
            ));
        }
        let sample_rate = decoded.rate().max(1) as f64;
        Ok(Self {
            duration: frames.len() as f64 / sample_rate,
            frames,
            sample_rate,
        })
    }

    #[inline]
    fn read(&self, position: f64) -> f64 {
        let frames = self.frames.len();
        if frames == 0 || !position.is_finite() {
            return 0.0;
        }
        let index = position.floor();
        if index < 0.0 || index >= (frames - 1) as f64 {
            return self
                .frames
                .get(index.max(0.0) as usize)
                .copied()
                .unwrap_or(0.0) as f64;
        }
        let fraction = position - index;
        let first = self.frames[index as usize] as f64;
        let second = self.frames[index as usize + 1] as f64;
        first + (second - first) * fraction
    }
}

#[derive(Debug, Clone, Copy)]
struct Grain {
    active: bool,
    position: f64,
    step: f64,
    age: f64,
    length: f64,
    gain: f64,
    pan: f64,
    window: GrainWindow,
    window_param: f64,
    window_skew: f64,
}

impl Default for Grain {
    fn default() -> Self {
        Self {
            active: false,
            position: 0.0,
            step: 1.0,
            age: 0.0,
            length: 1.0,
            gain: 0.0,
            pan: 0.0,
            window: GrainWindow::Hann,
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
        if !self.started {
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
        let density = (patch.density + density_offset).clamp(0.0, 4096.0);
        self.spawn_debt += density * dt;
        // Bound the catch-up loop; the pool is the real limit.
        if self.spawn_debt > MAX_GRAINS as f64 {
            self.spawn_debt = MAX_GRAINS as f64;
        }
        while self.spawn_debt >= 1.0 {
            self.spawn_debt -= 1.0;
            if let Some(index) = self.grains.iter().position(|grain| !grain.active) {
                self.spawn(source, patch, pitch_ratio, sample_rate, index);
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
            let window = grain_window(grain.window, ratio, grain.window_param, grain.window_skew);
            let value = source.read(grain.position) * window * grain.gain;
            left += value * (1.0 - grain.pan);
            right += value * (1.0 + grain.pan);
            grain.age += 1.0;
            grain.position += grain.step;
            if grain.position < 0.0 || grain.position >= source.frames.len() as f64 {
                grain.active = false;
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
    ) {
        let jitter = |this: &mut Self| this.uniform();
        let offset = (jitter(self) * 2.0 - 1.0) * patch.random_offset * patch.grain_length;
        let mut direction = if patch.reverse { -1.0 } else { 1.0 };
        if jitter(self) < patch.random_direction {
            direction = -direction;
        }
        let length = patch.grain_length * (1.0 + (jitter(self) * 2.0 - 1.0) * patch.random_length);
        let length = length.clamp(0.002, source.duration.max(0.002));
        let semitones = (jitter(self) * 2.0 - 1.0) * patch.random_pitch;
        let rate = pitch_ratio * 2f64.powf(semitones / 12.0) * direction;
        let gain = 1.0 - jitter(self) * patch.random_gain;
        let pan = (jitter(self) * 2.0 - 1.0) * patch.random_pan;
        let position = (self.playhead.seconds() + offset) * source.sample_rate;
        self.grains[index] = Grain {
            active: true,
            position,
            step: rate * source.sample_rate / sample_rate,
            age: 0.0,
            length: (length * sample_rate).max(1.0),
            gain: gain.clamp(0.0, 1.0),
            pan: pan.clamp(-1.0, 1.0),
            window: patch.window,
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
            frames,
            sample_rate: 44_100.0,
            duration: len as f64 / 44_100.0,
        }
    }

    fn patch() -> GranularPatch {
        GranularPatch {
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
