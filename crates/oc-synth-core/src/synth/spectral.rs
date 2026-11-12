//! Spectral source oscillator: load-time STFT analysis with per-block overlap-add.
//!
//! Analysis runs once when the patch loads. The callback only performs one
//! inverse FFT per voice per `HOP` samples into preallocated buffers, so it
//! never allocates, locks or starts a thread.

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};
use std::f64::consts::TAU;
use std::path::{Path, PathBuf};

use super::granular::{Playhead, SourceSample};

/// Analysis and synthesis FFT size.
pub const FFT_SIZE: usize = 1024;
/// Analysis and synthesis hop; also the resynthesis block length.
pub const HOP: usize = 256;
/// Positive-frequency bins including DC and Nyquist.
pub const BINS: usize = FFT_SIZE / 2 + 1;
/// Window-centre latency in samples (about 10.7 ms at 48 kHz).
pub const LATENCY_SAMPLES: usize = FFT_SIZE / 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Warp applied to the spectral source.
pub enum SpectralWarp {
    Off,
    /// Periodic comb over the magnitude axis.
    Comb,
    /// Alternating pass/stop bands.
    Gate,
    /// Magnitude blur.
    Smear,
    /// Stretch partials away from the spectrum centre.
    Spread,
    /// Sum two copies shifted in opposite directions.
    Detune,
    /// Scale the frequency axis by `2^morph`.
    PitchShift,
    /// Add an octave above every partial.
    AddHarmonics,
    /// Add an octave below every partial.
    AddSubharmonics,
    /// Log-frequency comb.
    Shepard,
    /// Move spectral peaks up by up to two octaves.
    PeakShift,
    /// Blend towards the mirrored spectrum.
    Mirror,
    /// Quadratic phase offset; magnitudes unchanged.
    PhaseTwist,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Analysis and playback settings for the spectral source.
pub struct SpectralPatch {
    /// Absolute path to the decoded source sample.
    pub sample: PathBuf,
    pub root_note: f64,
    /// Start position in seconds.
    pub position: f64,
    pub loop_start: f64,
    pub loop_end: f64,
    pub ping_pong: bool,
    /// Scan speed in source durations per second.
    pub scan_rate: f64,
    pub pitch_track: bool,
    /// First spectral warp and its 0..=1 amount.
    pub warp: SpectralWarp,
    pub morph: f64,
    /// Second spectral warp and its 0..=1 amount.
    pub warp2: SpectralWarp,
    pub morph2: f64,
    /// Frequency-axis bend in semitones.
    pub bend: f64,
    /// 0 = free-running phase, 1 = lock to the analysed phase.
    pub phase_lock: f64,
    /// Re-seed the phase accumulator on detected transients.
    pub transients: bool,
    /// Resynthesis band limits; 0 disables the edge.
    pub low_hz: f64,
    pub high_hz: f64,
    pub band_smooth: bool,
    pub seed: u64,
}

/// Load-time STFT of one source sample.
pub struct SpectralSource {
    pub sample_rate: f64,
    pub duration: f64,
    frames: usize,
    magnitudes: Vec<f32>,
    phases: Vec<f32>,
    /// Phase advance per hop in radians.
    advances: Vec<f32>,
    transients: Vec<bool>,
}

impl SpectralSource {
    /// Analyze a sample file into a spectral source.
    pub fn load(path: &Path) -> Result<Self, String> {
        let sample = SourceSample::load(path)?;
        Self::analyze(&sample)
    }

    fn analyze(sample: &SourceSample) -> Result<Self, String> {
        let frames = if sample.frames.len() <= FFT_SIZE {
            1
        } else {
            (sample.frames.len() - FFT_SIZE) / HOP + 1
        };
        let mut planner = FftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let window: Vec<f64> = (0..FFT_SIZE)
            .map(|index| {
                let phase = TAU * index as f64 / FFT_SIZE as f64;
                (0.5 - 0.5 * phase.cos()).sqrt()
            })
            .collect();
        let mut buffer = vec![Complex::new(0.0, 0.0); FFT_SIZE];
        let mut scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
        let mut magnitudes = vec![0.0f32; frames * BINS];
        let mut phases = vec![0.0f32; frames * BINS];
        let mut advances = vec![0.0f32; frames * BINS];
        let mut transients = vec![false; frames];
        let mut previous = vec![Complex::new(0.0, 0.0); BINS];
        for frame in 0..frames {
            let start = frame * HOP;
            for index in 0..FFT_SIZE {
                let value = sample.frames.get(start + index).copied().unwrap_or(0.0) as f64;
                buffer[index] = Complex::new(value * window[index], 0.0);
            }
            fft.process_with_scratch(&mut buffer, &mut scratch);
            let mut flux = 0.0;
            let mut total = 0.0;
            for bin in 0..BINS {
                let value = buffer[bin];
                let magnitude = value.norm();
                let phase = value.arg();
                let index = frame * BINS + bin;
                magnitudes[index] = magnitude as f32;
                phases[index] = phase as f32;
                let expected = TAU * bin as f64 * HOP as f64 / FFT_SIZE as f64;
                advances[index] = if frame == 0 {
                    expected as f32
                } else {
                    let delta = (value * previous[bin].conj()).arg();
                    let wrapped = wrap_pi(delta - expected);
                    (expected + wrapped) as f32
                };
                if frame > 0 {
                    let previous_magnitude = magnitudes[(frame - 1) * BINS + bin] as f64;
                    flux += (magnitude - previous_magnitude).max(0.0);
                    total += magnitude;
                }
                previous[bin] = value;
            }
            transients[frame] = frame > 0 && flux > total * 0.35;
        }
        Ok(Self {
            sample_rate: sample.sample_rate,
            duration: sample.duration,
            frames,
            magnitudes,
            phases,
            advances,
            transients,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SpectralFrame {
    magnitude: f64,
    phase: f64,
    advance: f64,
}

impl SpectralSource {
    fn frame_at(&self, position_seconds: f64, bin: usize) -> SpectralFrame {
        let frame =
            (position_seconds * self.sample_rate / HOP as f64).clamp(0.0, (self.frames - 1) as f64);
        let first = frame.floor() as usize;
        let second = (first + 1).min(self.frames - 1);
        let mix = frame - first as f64;
        let at = |index: usize| index * BINS + bin;
        SpectralFrame {
            magnitude: lerp(
                self.magnitudes[at(first)] as f64,
                self.magnitudes[at(second)] as f64,
                mix,
            ),
            phase: lerp_angle(
                self.phases[at(first)] as f64,
                self.phases[at(second)] as f64,
                mix,
            ),
            advance: lerp(
                self.advances[at(first)] as f64,
                self.advances[at(second)] as f64,
                mix,
            ),
        }
    }

    fn transient_at(&self, position_seconds: f64) -> bool {
        let frame = (position_seconds * self.sample_rate / HOP as f64)
            .round()
            .clamp(0.0, (self.frames - 1) as f64) as usize;
        self.transients[frame]
    }
}

/// Per-voice phase accumulator and overlap-add ring.
#[derive(Debug, Clone)]
pub struct SpectralVoice {
    phase: [f64; BINS],
    ring: [f64; FFT_SIZE],
    block: [f64; HOP],
    block_pos: usize,
    write: usize,
    playhead: Playhead,
    time: f64,
    started: bool,
    first_block: bool,
    spectrum: [Complex<f64>; FFT_SIZE],
    scratch: [Complex<f64>; FFT_SIZE],
    magnitudes: [f64; BINS],
    warp_scratch: [f64; BINS],
}

impl Default for SpectralVoice {
    fn default() -> Self {
        Self::new()
    }
}

impl SpectralVoice {
    /// Create a silent spectral voice.
    pub fn new() -> Self {
        Self {
            phase: [0.0; BINS],
            ring: [0.0; FFT_SIZE],
            block: [0.0; HOP],
            block_pos: HOP,
            write: 0,
            playhead: Playhead::new(0.0),
            time: 0.0,
            started: false,
            first_block: true,
            spectrum: [Complex::new(0.0, 0.0); FFT_SIZE],
            scratch: [Complex::new(0.0, 0.0); FFT_SIZE],
            magnitudes: [0.0; BINS],
            warp_scratch: [0.0; BINS],
        }
    }

    /// Restore the state [`SpectralVoice::new`] builds without reallocating.
    pub fn reset(&mut self) {
        self.phase.fill(0.0);
        self.ring.fill(0.0);
        self.block.fill(0.0);
        self.block_pos = HOP;
        self.write = 0;
        self.playhead = Playhead::new(0.0);
        self.time = 0.0;
        self.started = false;
        self.first_block = true;
        self.spectrum.fill(Complex::new(0.0, 0.0));
        self.scratch.fill(Complex::new(0.0, 0.0));
        self.magnitudes.fill(0.0);
        self.warp_scratch.fill(0.0);
    }

    /// Largest scratch length the inverse plan may ask for.
    pub fn scratch_capacity() -> usize {
        FFT_SIZE
    }

    #[allow(clippy::too_many_arguments)]
    /// Render one block for a source and patch.
    pub fn render(
        &mut self,
        source: &SpectralSource,
        patch: &SpectralPatch,
        pitch_ratio: f64,
        morph_offset: f64,
        bend_offset: f64,
        sample_rate: f64,
        ifft: &dyn Fft<f64>,
    ) -> f64 {
        if !self.started {
            self.playhead = Playhead::new(patch.position);
            self.started = true;
        }
        if self.block_pos >= HOP {
            self.refill(
                source,
                patch,
                pitch_ratio,
                morph_offset,
                bend_offset,
                sample_rate,
                ifft,
            );
            self.block_pos = 0;
        }
        let value = self.block[self.block_pos];
        self.block_pos += 1;
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
        value
    }

    #[allow(clippy::too_many_arguments)]
    fn refill(
        &mut self,
        source: &SpectralSource,
        patch: &SpectralPatch,
        pitch_ratio: f64,
        morph_offset: f64,
        bend_offset: f64,
        sample_rate: f64,
        ifft: &dyn Fft<f64>,
    ) {
        let position = self.playhead.seconds();
        for bin in 0..BINS {
            self.magnitudes[bin] = source.frame_at(position, bin).magnitude;
        }
        let (morph, bend) = (
            (patch.morph + morph_offset).clamp(0.0, 1.0),
            (patch.bend + bend_offset).clamp(-48.0, 48.0),
        );
        apply_warp(
            patch.warp,
            &mut self.magnitudes,
            &mut self.warp_scratch,
            morph,
        );
        let morph2 = patch.morph2.clamp(0.0, 1.0);
        apply_warp(
            patch.warp2,
            &mut self.magnitudes,
            &mut self.warp_scratch,
            morph2,
        );
        let ratio = pitch_ratio * 2f64.powf(bend / 12.0);
        let twist = if patch.warp == SpectralWarp::PhaseTwist {
            morph * 4.0
        } else if patch.warp2 == SpectralWarp::PhaseTwist {
            morph2 * 4.0
        } else {
            0.0
        };
        let transient = patch.transients && source.transient_at(position);
        self.spectrum.fill(Complex::new(0.0, 0.0));
        for bin in 0..BINS {
            let frequency = bin as f64 * sample_rate / FFT_SIZE as f64;
            let band = band_gain(frequency, patch);
            if band <= 0.0 {
                continue;
            }
            let source_bin = bin as f64 / ratio;
            if source_bin >= (BINS - 1) as f64 {
                continue;
            }
            let index = source_bin.floor() as usize;
            let fraction = source_bin - index as f64;
            let magnitude = lerp(
                self.magnitudes[index],
                self.magnitudes[(index + 1).min(BINS - 1)],
                fraction,
            ) * band;
            let frame = source.frame_at(position, index);
            // The accumulator carries the partial phase between hops; snapping
            // it back to the absolute analysis phase every block cancels the
            // advance when the playhead is frozen. Lock only on the first block
            // (or on a transient), then track the instantaneous frequency.
            if self.first_block {
                self.phase[bin] = patch.phase_lock.clamp(0.0, 1.0) * frame.phase;
            } else {
                self.phase[bin] += frame.advance * ratio;
            }
            if transient {
                self.phase[bin] = frame.phase;
            }
            let twist_phase = twist * (bin as f64 / BINS as f64).powi(2) * TAU;
            let phase = self.phase[bin] + twist_phase;
            self.spectrum[bin] = Complex::new(magnitude * phase.cos(), magnitude * phase.sin());
            if bin > 0 && bin < FFT_SIZE / 2 {
                self.spectrum[FFT_SIZE - bin] = self.spectrum[bin].conj();
            }
        }
        self.first_block = false;
        ifft.process_with_scratch(&mut self.spectrum, &mut self.scratch);
        let write = self.write;
        // The inverse FFT is unnormalised and the two sqrt-Hann windows give a
        // Hann sum of 2 at 75% overlap, so scale by 1/(2 * FFT_SIZE).
        let normalization = 0.5 / FFT_SIZE as f64;
        for index in 0..FFT_SIZE {
            let window = synthesis_window(index);
            let slot = (write + index) % FFT_SIZE;
            self.ring[slot] += self.spectrum[index].re * window * normalization;
        }
        for index in 0..HOP {
            let slot = (write + index) % FFT_SIZE;
            self.block[index] = self.ring[slot];
            self.ring[slot] = 0.0;
        }
        self.write = (write + HOP) % FFT_SIZE;
    }
}

fn band_gain(frequency: f64, patch: &SpectralPatch) -> f64 {
    let low = patch.low_hz.max(0.0);
    let high = if patch.high_hz > 0.0 {
        patch.high_hz
    } else {
        f64::INFINITY
    };
    let edge = |value: f64, corner: f64| {
        if patch.band_smooth {
            let octave = (value / corner).log2();
            (octave * 3.0 + 0.5).clamp(0.0, 1.0)
        } else if value >= corner {
            1.0
        } else {
            0.0
        }
    };
    let low_gain = if low > 0.0 { edge(frequency, low) } else { 1.0 };
    let high_gain = if high.is_finite() {
        1.0 - edge(frequency, high)
    } else {
        1.0
    };
    low_gain * high_gain
}

fn apply_warp(
    warp: SpectralWarp,
    magnitudes: &mut [f64; BINS],
    scratch: &mut [f64; BINS],
    morph: f64,
) {
    if morph <= 0.0 || warp == SpectralWarp::Off || warp == SpectralWarp::PhaseTwist {
        return;
    }
    scratch.copy_from_slice(magnitudes);
    match warp {
        SpectralWarp::Off | SpectralWarp::PhaseTwist => {}
        SpectralWarp::Comb => {
            let period = 1.0 + (1.0 - morph) * 32.0;
            for (bin, magnitude) in magnitudes.iter_mut().enumerate() {
                let gain = 0.5 + 0.5 * (TAU * bin as f64 / period).cos();
                *magnitude *= gain;
            }
        }
        SpectralWarp::Gate => {
            let period = 1.0 + (1.0 - morph) * 16.0;
            for (bin, magnitude) in magnitudes.iter_mut().enumerate() {
                let phase = (bin as f64 / period).fract();
                *magnitude *= if phase < morph { 1.0 } else { 0.02 };
            }
        }
        SpectralWarp::Smear => {
            let radius = (morph * 48.0).round() as usize;
            if radius > 0 {
                let mut prefix = [0.0; BINS + 1];
                for bin in 0..BINS {
                    prefix[bin + 1] = prefix[bin] + scratch[bin];
                }
                for (bin, magnitude) in magnitudes.iter_mut().enumerate() {
                    let start = bin.saturating_sub(radius);
                    let end = (bin + radius + 1).min(BINS);
                    *magnitude = (prefix[end] - prefix[start]) / (end - start) as f64;
                }
            }
        }
        SpectralWarp::Spread => {
            let centre = BINS as f64 * 0.5;
            for (bin, magnitude) in magnitudes.iter_mut().enumerate() {
                let source = centre + (bin as f64 - centre) / (1.0 + 2.0 * morph);
                *magnitude = sample_bins(scratch, source);
            }
        }
        SpectralWarp::Detune => {
            let shift = (morph * 12.0).round() as usize;
            for (bin, magnitude) in magnitudes.iter_mut().enumerate() {
                let low = sample_bins(scratch, bin as f64 - shift as f64);
                let high = sample_bins(scratch, bin as f64 + shift as f64);
                *magnitude = 0.5 * (low + high);
            }
        }
        SpectralWarp::PitchShift => {
            let ratio = 2f64.powf(morph);
            for (bin, magnitude) in magnitudes.iter_mut().enumerate() {
                *magnitude = sample_bins(scratch, bin as f64 / ratio);
            }
        }
        SpectralWarp::AddHarmonics => {
            for bin in 0..BINS {
                magnitudes[bin] =
                    scratch[bin] + 0.5 * morph * sample_bins(scratch, bin as f64 * 0.5);
            }
        }
        SpectralWarp::AddSubharmonics => {
            for bin in 0..BINS {
                magnitudes[bin] =
                    scratch[bin] + 0.5 * morph * sample_bins(scratch, bin as f64 * 2.0);
            }
        }
        SpectralWarp::Shepard => {
            for (bin, magnitude) in magnitudes.iter_mut().enumerate() {
                let frequency = (bin as f64 + 1.0).max(1.0);
                let gain = 0.5 + 0.5 * (TAU * frequency.log2() * morph * 4.0).cos();
                *magnitude *= gain;
            }
        }
        SpectralWarp::PeakShift => {
            let ratio = 2f64.powf(morph * 2.0);
            for (bin, magnitude) in magnitudes.iter_mut().enumerate() {
                *magnitude = sample_bins(scratch, bin as f64 / ratio);
            }
        }
        SpectralWarp::Mirror => {
            for bin in 0..BINS {
                let mirror = BINS - 1 - bin;
                magnitudes[bin] = (1.0 - morph) * scratch[bin] + morph * scratch[mirror];
            }
        }
    }
}

#[inline]
fn sample_bins(magnitudes: &[f64; BINS], position: f64) -> f64 {
    if position <= 0.0 || position >= (BINS - 1) as f64 {
        return 0.0;
    }
    let index = position.floor() as usize;
    let fraction = position - index as f64;
    lerp(magnitudes[index], magnitudes[index + 1], fraction)
}

fn synthesis_windows() -> &'static [f64; FFT_SIZE] {
    static WINDOW: std::sync::OnceLock<[f64; FFT_SIZE]> = std::sync::OnceLock::new();
    WINDOW.get_or_init(|| {
        std::array::from_fn(|index| {
            let phase = TAU * index as f64 / FFT_SIZE as f64;
            (0.5 - 0.5 * phase.cos()).sqrt()
        })
    })
}

#[inline]
fn synthesis_window(index: usize) -> f64 {
    synthesis_windows()[index]
}

#[inline]
fn lerp(first: f64, second: f64, mix: f64) -> f64 {
    first + (second - first) * mix
}

#[inline]
fn wrap_pi(value: f64) -> f64 {
    let mut value = value % TAU;
    if value > TAU * 0.5 {
        value -= TAU;
    } else if value < -TAU * 0.5 {
        value += TAU;
    }
    value
}

fn lerp_angle(first: f64, second: f64, mix: f64) -> f64 {
    first + wrap_pi(second - first) * mix
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(frequency: f64) -> SourceSample {
        let rate = 44_100.0;
        let len = 44_100;
        let frames = (0..len)
            .map(|index| {
                let phase = TAU * frequency * index as f64 / rate;
                (phase.sin() * 0.5) as f32
            })
            .collect();
        SourceSample {
            frames,
            sample_rate: rate,
            duration: len as f64 / rate,
        }
    }

    fn analysis(sample: &SourceSample) -> SpectralSource {
        SpectralSource::analyze(sample).unwrap()
    }

    fn patch() -> SpectralPatch {
        SpectralPatch {
            sample: PathBuf::from("fixture.wav"),
            root_note: 60.0,
            position: 0.0,
            loop_start: 0.0,
            loop_end: 0.0,
            ping_pong: false,
            scan_rate: 0.0,
            pitch_track: true,
            warp: SpectralWarp::Off,
            morph: 0.0,
            warp2: SpectralWarp::Off,
            morph2: 0.0,
            bend: 0.0,
            phase_lock: 1.0,
            transients: false,
            low_hz: 0.0,
            high_hz: 0.0,
            band_smooth: false,
            seed: 1,
        }
    }

    fn render(source: &SpectralSource, patch: &SpectralPatch, frames: usize) -> Vec<f64> {
        let ifft = FftPlanner::<f64>::new().plan_fft_inverse(FFT_SIZE);
        let mut voice = SpectralVoice::new();
        let mut out = Vec::with_capacity(frames);
        for _ in 0..frames {
            out.push(voice.render(source, patch, 1.0, 0.0, 0.0, 48_000.0, &*ifft));
        }
        out
    }

    fn rms(values: &[f64]) -> f64 {
        (values.iter().map(|value| value * value).sum::<f64>() / values.len() as f64).sqrt()
    }

    #[test]
    fn render_is_deterministic() {
        let source = analysis(&source(441.0));
        let patch = patch();
        let first = render(&source, &patch, 8_192);
        let second = render(&source, &patch, 8_192);
        assert_eq!(first, second);
        assert!(rms(&first) > 1.0e-4);
    }

    /// Note-on resets an existing engine; that must equal building a new one.
    #[test]
    fn reset_restores_a_fresh_voice() {
        let source = analysis(&source(441.0));
        let patch = patch();
        let ifft = FftPlanner::<f64>::new().plan_fft_inverse(FFT_SIZE);
        let mut reused = SpectralVoice::new();
        for _ in 0..3_000 {
            reused.render(&source, &patch, 1.0, 0.0, 0.0, 48_000.0, &*ifft);
        }
        reused.reset();
        let mut fresh = SpectralVoice::new();
        let mut reused_out = Vec::with_capacity(8_192);
        let mut fresh_out = Vec::with_capacity(8_192);
        for _ in 0..8_192 {
            reused_out.push(reused.render(&source, &patch, 1.0, 0.0, 0.0, 48_000.0, &*ifft));
            fresh_out.push(fresh.render(&source, &patch, 1.0, 0.0, 0.0, 48_000.0, &*ifft));
        }
        assert_eq!(reused_out, fresh_out);
    }

    #[test]
    fn zero_morph_preserves_source_level() {
        let sample = source(441.0);
        let source = analysis(&sample);
        let patch = patch();
        let output = render(&source, &patch, 24_000);
        let input = rms(&sample.frames[4_096..20_096]
            .iter()
            .map(|value| *value as f64)
            .collect::<Vec<_>>());
        let ratio = rms(&output[4_096..20_096]) / input;
        assert!(
            (ratio - 1.0).abs() < 0.25,
            "spectral resynthesis level ratio {ratio}"
        );
    }

    #[test]
    fn morph_moves_the_spectral_centroid() {
        let source = analysis(&source(441.0));
        let mut shifted = patch();
        shifted.warp = SpectralWarp::PitchShift;
        shifted.morph = 1.0;
        let base = render(&source, &patch(), 24_000);
        let moved = render(&source, &shifted, 24_000);
        assert!(centroid(&moved) > centroid(&base) * 1.5);
    }

    fn centroid(values: &[f64]) -> f64 {
        let frames = 2_048;
        let mut planner = FftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(frames);
        let mut buffer: Vec<Complex<f64>> = (0..frames)
            .map(|index| Complex::new(values[values.len() - frames + index], 0.0))
            .collect();
        fft.process(&mut buffer);
        let mut weighted = 0.0;
        let mut total = 0.0;
        for (bin, value) in buffer.iter().enumerate().take(frames / 2) {
            let magnitude = value.norm();
            weighted += magnitude * bin as f64 * 48_000.0 / frames as f64;
            total += magnitude;
        }
        weighted / total.max(1.0e-12)
    }
}
