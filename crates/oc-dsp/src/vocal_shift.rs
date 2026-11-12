//! Offline TD-PSOLA pitch shifter driven by a per-segment cents curve.
//!
//! Pure buffer in, buffer out: no files, no engine types, no allocation shared
//! with the caller. The analysis reuses the realtime detector (16 kHz YIN plus
//! a median period track) and the grain primitives in [`super::psola`];
//! synthesis is one whole-buffer overlap-add against the detected pitch marks,
//! so every channel gets the same grain plan and a zero-cents span is copied
//! bit for bit.
//!
//! Conventions: `frames` are samples per channel, `cents` is positive for a
//! lift, and the curve is indexed in output frames, which are also input
//! frames because the pass never changes length. A segment carries an
//! independent formant displacement: the take's envelope is reshaped first,
//! then the PSOLA path shifts the pitch of the already-shaped signal. When the
//! formant is zero the reshape is skipped and the output is the pitch-only
//! shifter's, bit for bit.
//!
//! [`time_warp`] is the companion pass for note timing: it moves and stretches
//! note spans inside a fixed-length buffer by repeating or dropping pitch
//! periods (pitch unchanged), erases the vacated spans and fills them from the
//! take's own room. An empty edit list returns the input bit for bit, so a
//! project with no timing decisions takes exactly the pre-timing path.

use std::f64::consts::PI;

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

use super::psola;
use super::tuner::{Decimator, FULL_WINDOW_S, MAX_F0, MIN_F0, PeriodTracker, YinDetector};

/// Largest shift the grain policy is designed for (±12 semitones).
pub const MAX_SHIFT_CENTS: f32 = 1_200.0;

/// Bump when a change to this module can move an output sample; bakes and
/// caches outside it key on this value and must miss after a bump.
pub const ALGO_VERSION: u32 = 3;

/// Largest formant shift the envelope pass is designed for (±12 semitones).
/// Beyond this the request is clamped; the reshape stays finite but the
/// envelope stops being a faithful vocal-tract move.
pub const MAX_FORMANT_CENTS: f32 = 1_200.0;

/// Largest formant shift the acceptance table keeps inside 5 %: beyond ±300
/// the LPC check fails (-8.9 % at -700) and at +1200 the sideband budget is
/// broken (-52.8 dBc). Callers clamp here and warn; the DSP accepts the range.
pub const HONEST_FORMANT_CENTS: f32 = 300.0;

/// Envelope analysis window, in seconds at the input rate, rounded up to a
/// power of two (42.7 ms at 48 kHz). Long enough for ~6 periods of a 140 Hz
/// voice, short enough to follow a consonant. The estimate is taken from the
/// unshifted take: the PSOLA path preserves that envelope, and measuring the
/// shifted comb instead splits F2 into grain sidebands.
const FORMANT_WINDOW_S: f64 = 0.0427;
/// STFT hop as a fraction of the envelope window (5.3 ms at 48 kHz).
const FORMANT_HOP_DIVISOR: usize = 8;
/// Cepstral lifter: quefrency below this is envelope, above it is excitation.
/// 4 ms resolves formants ~250 Hz apart and still rejects the pitch comb of a
/// 140 Hz voice (7.1 ms).
const FORMANT_LIFTER_S: f64 = 0.004;
/// Largest envelope correction in dB per bin; deeper cuts would invert or
/// hollow the spectrum.
const FORMANT_GAIN_LIMIT_DB: f64 = 24.0;
/// Envelope floor relative to the frame peak, in dB.
const FORMANT_ENVELOPE_FLOOR_DB: f64 = -80.0;
/// Frame peak below which the envelope is left alone.
const FORMANT_SILENCE_PEAK: f64 = 1e-4;

/// Detector hop, matching the realtime tracker.
const ANALYSIS_HOP_S: f64 = 0.005;
/// Peak search half-width as a fraction of the predicted period.
const MARK_WINDOW: f64 = 0.35;
/// Shortest period the epoch grid will use, in samples.
const MIN_PERIOD: f64 = 2.0;
/// Smoothed level below which a pitch mark is not trusted.
const SILENCE_PEAK: f64 = 1e-4;
/// Window weight below which a sample is not worth accumulating.
const WEIGHT_FLOOR: f64 = 1e-6;
/// Default curve slew into a new value.
const RAMP_S: f64 = 0.010;
/// Default wet/dry crossfade at the edge of a shifted run.
const CROSSFADE_S: f64 = 0.002;
/// Unvoiced frames that may sit inside one voiced run.
const BRIDGE_FRAMES: usize = 2;
/// Slew of the time pass into and out of a moved note.
const TIME_EDGE_FADE_S: f64 = 0.002;
/// Crossfade where the erased span meets the take's room.
const TIME_FILL_FADE_S: f64 = 0.010;
/// Shortest room window the fill will take; shorter takes fade to silence.
const TIME_MIN_ROOM_S: f64 = 0.010;

/// One constant-cents span, half open (`start..end`) in frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShiftSegment {
    pub start: usize,
    pub end: usize,
    /// Pitch displacement in cents; positive lifts.
    pub cents: f32,
    /// Independent formant displacement in cents; zero keeps the envelope.
    /// The envelope moves by this ratio regardless of `cents`.
    pub formant_cents: f32,
}

impl ShiftSegment {
    /// A pitch-only segment; the formant stays where it is.
    pub fn new(start: usize, end: usize, cents: f32) -> Self {
        Self {
            start,
            end,
            cents,
            formant_cents: 0.0,
        }
    }
}

/// The shift curve: sorted, non-overlapping segments; frames outside them are
/// zero (= pass the input through untouched).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ShiftCurve {
    frames: usize,
    segments: Vec<ShiftSegment>,
    /// Per-segment slew in frames, parallel to `segments`; an empty vector or a
    /// `None` entry uses [`ShiftConfig::ramp_frames`].
    ramps: Vec<Option<usize>>,
}

impl ShiftCurve {
    /// A curve that leaves the buffer alone.
    pub fn none(frames: usize) -> Self {
        Self {
            frames,
            segments: Vec::new(),
            ramps: Vec::new(),
        }
    }

    /// One constant shift over the whole buffer.
    pub fn constant(frames: usize, cents: f32) -> Self {
        let cents = cents.clamp(-MAX_SHIFT_CENTS, MAX_SHIFT_CENTS);
        if frames == 0 || cents == 0.0 {
            return Self::none(frames);
        }
        Self {
            frames,
            segments: vec![ShiftSegment::new(0, frames, cents)],
            ramps: Vec::new(),
        }
    }

    /// One constant pitch shift and one constant formant shift over the
    /// whole buffer. Zero for both is the same curve as [`Self::none`].
    pub fn constant_with_formant(frames: usize, cents: f32, formant_cents: f32) -> Self {
        let cents = cents.clamp(-MAX_SHIFT_CENTS, MAX_SHIFT_CENTS);
        let formant_cents = formant_cents.clamp(-MAX_FORMANT_CENTS, MAX_FORMANT_CENTS);
        if frames == 0 || (cents == 0.0 && formant_cents == 0.0) {
            return Self::none(frames);
        }
        Self {
            frames,
            segments: vec![ShiftSegment {
                start: 0,
                end: frames,
                cents,
                formant_cents,
            }],
            ramps: Vec::new(),
        }
    }

    /// Build from segments; they are reordered by start and clipped to
    /// `frames`. Returns an error for overlaps, empty spans or non-finite
    /// cents.
    pub fn from_segments(frames: usize, segments: &[ShiftSegment]) -> Result<Self, ShiftError> {
        Self::build(frames, segments.iter().map(|segment| (*segment, None)))
    }

    /// Build from segments that also carry their own slew.
    ///
    /// `ramp` is in frames and overrides [`ShiftConfig::ramp_frames`] for that
    /// segment; `None` keeps the call-wide default. Only the enter and the
    /// exit of a segment use the override, so a per-note glide does not change
    /// how the rest of the take is slewed.
    pub fn from_ramped_segments(
        frames: usize,
        segments: &[(ShiftSegment, Option<usize>)],
    ) -> Result<Self, ShiftError> {
        Self::build(frames, segments.iter().copied())
    }

    fn build(
        frames: usize,
        entries: impl Iterator<Item = (ShiftSegment, Option<usize>)>,
    ) -> Result<Self, ShiftError> {
        let mut ordered: Vec<(ShiftSegment, Option<usize>)> = Vec::new();
        for (segment, ramp) in entries {
            if !segment.cents.is_finite() {
                return Err(ShiftError::NonFiniteCents);
            }
            if segment.start >= segment.end {
                return Err(ShiftError::EmptySegment);
            }
            if segment.start >= frames {
                continue;
            }
            let clipped = ShiftSegment {
                start: segment.start,
                end: segment.end.min(frames),
                cents: segment.cents.clamp(-MAX_SHIFT_CENTS, MAX_SHIFT_CENTS),
                formant_cents: segment
                    .formant_cents
                    .clamp(-MAX_FORMANT_CENTS, MAX_FORMANT_CENTS),
            };
            ordered.push((clipped, ramp));
        }
        ordered.sort_by_key(|(segment, _)| segment.start);
        for pair in ordered.windows(2) {
            if pair[0].0.end > pair[1].0.start {
                return Err(ShiftError::OverlappingSegments);
            }
        }
        ordered.retain(|(segment, _)| segment.cents != 0.0 || segment.formant_cents != 0.0);
        let ramps: Vec<Option<usize>> = ordered.iter().map(|(_, ramp)| *ramp).collect();
        let segments: Vec<ShiftSegment> = ordered.into_iter().map(|(segment, _)| segment).collect();
        Ok(Self {
            frames,
            segments,
            ramps,
        })
    }

    /// Frames per channel the curve was built for.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Segments in ascending order.
    pub fn segments(&self) -> &[ShiftSegment] {
        &self.segments
    }

    /// Target cents at `frame` without the synthesis ramp.
    pub fn cents_at(&self, frame: usize) -> f32 {
        self.segments
            .iter()
            .find(|segment| frame >= segment.start && frame < segment.end)
            .map_or(0.0, |segment| segment.cents)
    }

    /// Target formant cents at `frame` without the synthesis ramp.
    pub fn formant_at(&self, frame: usize) -> f32 {
        self.segments
            .iter()
            .find(|segment| frame >= segment.start && frame < segment.end)
            .map_or(0.0, |segment| segment.formant_cents)
    }

    /// True when the whole buffer would pass through untouched.
    pub fn is_silent(&self) -> bool {
        self.segments.is_empty()
    }

    /// Expand to one value per frame with the synthesis slew applied.
    ///
    /// A change *into* a new value ramps over [`ShiftConfig::ramp_frames`]
    /// samples after the change, or over the segment's own slew when the curve
    /// carries one; a change *to* zero ramps down over the same span before
    /// it. Zero spans therefore stay exactly zero, which is what lets them
    /// pass through bit for bit.
    fn ramped(&self, ramp: usize) -> Vec<f64> {
        self.ramped_field(ramp, |segment| segment.cents)
    }

    /// Formant curve with the same slew rules as the pitch curve.
    fn formant_ramped(&self, ramp: usize) -> Vec<f64> {
        self.ramped_field(ramp, |segment| segment.formant_cents)
    }

    fn ramped_field(&self, ramp: usize, value: impl Fn(&ShiftSegment) -> f32) -> Vec<f64> {
        let mut out = vec![0.0_f64; self.frames];
        for segment in &self.segments {
            out[segment.start..segment.end].fill(f64::from(value(segment)));
        }
        for (index, segment) in self.segments.iter().enumerate() {
            let slew = self.ramps.get(index).copied().flatten().unwrap_or(ramp);
            if slew == 0 {
                continue;
            }
            let width = slew.min(segment.end - segment.start);
            let after = out[segment.start];
            if segment.start > 0 {
                let before = out[segment.start - 1];
                if before != after {
                    for offset in 0..width {
                        out[segment.start + offset] =
                            before + (after - before) * ramp_weight(offset, width);
                    }
                }
            }
            let followed = self
                .segments
                .get(index + 1)
                .is_some_and(|next| next.start == segment.end);
            if segment.end < self.frames && !followed {
                let before = out[segment.end - 1];
                for offset in 0..width {
                    out[segment.end - width + offset] = before * (1.0 - ramp_weight(offset, width));
                }
            }
        }
        out
    }
}

/// Raised-cosine weight rising from 0 to 1 across `width` samples.
fn ramp_weight(offset: usize, width: usize) -> f64 {
    let phase = PI * (offset as f64 + 0.5) / width as f64;
    0.5 - 0.5 * phase.cos()
}

/// Grain policy and boundary handling. Defaults are the measured values in
/// `notes/vocal3.md`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShiftConfig {
    /// Slew into a new cents value, in frames (default 10 ms).
    pub ramp_frames: usize,
    /// Wet/dry crossfade at the edge of a shifted run, in frames (default 2 ms).
    pub crossfade_frames: usize,
    /// Window length in periods (default 1.0, i.e. a two-period Hann grain).
    pub grain_scale: f32,
    /// Follow the measured period contour (true) or flatten it (false).
    pub preserve_period_contour: bool,
    /// Envelope FFT length in frames (default 42.7 ms, power of two).
    pub formant_window_frames: usize,
    /// Envelope hop in frames (default an eighth of the window).
    pub formant_hop_frames: usize,
    /// Cepstral lifter length in frames (default 4 ms).
    pub formant_lifter_frames: usize,
    /// Largest envelope correction per bin, in dB (default 24).
    pub formant_gain_limit_db: f64,
}

impl ShiftConfig {
    /// Defaults for `sample_rate`, rounded to whole frames.
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let mut formant_window_frames = (rate * FORMANT_WINDOW_S).round().max(16.0) as usize;
        formant_window_frames = formant_window_frames.next_power_of_two();
        Self {
            ramp_frames: (rate * RAMP_S).round() as usize,
            crossfade_frames: (rate * CROSSFADE_S).round() as usize,
            grain_scale: 1.0,
            preserve_period_contour: true,
            formant_window_frames,
            formant_hop_frames: formant_window_frames / FORMANT_HOP_DIVISOR,
            formant_lifter_frames: (rate * FORMANT_LIFTER_S).round().max(2.0) as usize,
            formant_gain_limit_db: FORMANT_GAIN_LIMIT_DB,
        }
    }
}

impl Default for ShiftConfig {
    fn default() -> Self {
        Self::new(48_000)
    }
}

/// Why a shift could not be produced. Every unsupported buffer is rejected up
/// front; silence, noise and very short buffers are not errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftError {
    /// Only mono and stereo are supported.
    ChannelCount(usize),
    ZeroSampleRate,
    /// The interleaved length is not a whole number of frames.
    RaggedBuffer,
    /// The input contains NaN or infinity.
    NonFiniteInput,
    /// The curve was built for a different frame count.
    CurveLength {
        curve: usize,
        frames: usize,
    },
    NonFiniteCents,
    EmptySegment,
    OverlappingSegments,
    /// A time edit with an empty source or target span.
    EmptyEdit,
    /// A time edit whose source or target reaches outside the buffer.
    EditOutOfRange,
    /// Two time edits claim overlapping source or target spans.
    OverlappingEdits,
}

impl std::fmt::Display for ShiftError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChannelCount(count) => write!(formatter, "unsupported channel count {count}"),
            Self::ZeroSampleRate => write!(formatter, "sample rate must be positive"),
            Self::RaggedBuffer => write!(formatter, "buffer length is not a whole frame count"),
            Self::NonFiniteInput => write!(formatter, "input contains NaN or infinity"),
            Self::CurveLength { curve, frames } => {
                write!(
                    formatter,
                    "curve covers {curve} frames, buffer has {frames}"
                )
            }
            Self::NonFiniteCents => write!(formatter, "segment cents is not finite"),
            Self::EmptySegment => write!(formatter, "segment start must be before its end"),
            Self::OverlappingSegments => write!(formatter, "segments overlap"),
            Self::EmptyEdit => write!(formatter, "time edit start must be before its end"),
            Self::EditOutOfRange => write!(formatter, "time edit reaches outside the buffer"),
            Self::OverlappingEdits => write!(formatter, "time edits overlap"),
        }
    }
}

impl std::error::Error for ShiftError {}

/// What the pass did, including the worst transient it produced.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ShiftReport {
    pub frames: usize,
    /// Frames with a non-zero target after the ramp.
    pub shifted_frames: usize,
    /// Frames actually resynthesised (shifted and voiced).
    pub wet_frames: usize,
    /// Frames the detector called voiced.
    pub voiced_frames: usize,
    /// Largest output sample step, all channels.
    pub output_max_step: f32,
    /// Largest input sample step, all channels.
    pub input_max_step: f32,
    /// Worst sample step within 20 ms of any boundary (wet/dry edge or
    /// segment edge), over the input's own worst step in the same window.
    /// A ramp keeps it near one; a hard switch or a click pushes it up.
    pub boundary_step_ratio: f32,
    /// Largest absolute output sample.
    pub peak: f32,
    /// Frames with a non-zero formant target after the ramp.
    pub formant_frames: usize,
    /// Frames where the envelope correction hit its dB clamp. A large count
    /// means the request was larger than the analysis could honour.
    pub formant_limited_frames: usize,
    /// Largest envelope correction actually applied, in dB.
    pub formant_max_gain_db: f32,
}

/// Shifted buffer plus its report.
#[derive(Debug, Clone, PartialEq)]
pub struct ShiftOutput {
    pub samples: Vec<f32>,
    pub report: ShiftReport,
}

/// Apply the spectral-envelope stage alone to a finite interleaved stereo buffer.
pub fn shift_stereo_formants(samples: &mut [f32], sample_rate: u32, cents: f32) {
    shift_stereo_formants_while(samples, sample_rate, cents, || true);
}

/// Apply formant shifting while the caller still needs the result. On
/// interruption, returns false and the caller must discard the partial buffer.
pub fn shift_stereo_formants_while(
    samples: &mut [f32],
    sample_rate: u32,
    cents: f32,
    mut keep_going: impl FnMut() -> bool,
) -> bool {
    if cents == 0.0 || samples.is_empty() {
        return true;
    }
    let frames = samples.len() / 2;
    let config = ShiftConfig::new(sample_rate);
    // A clip can contain several vowels and consonants. Keep one static filter
    // per local region and crossfade filters so changes do not reset to dry.
    let hop = (f64::from(sample_rate) * 0.20).round().max(1.0) as usize;
    let overlap = (hop / 5).max(1);
    let cents = cents.clamp(-MAX_FORMANT_CENTS, MAX_FORMANT_CENTS);
    let voice_track = if cents.abs() > HONEST_FORMANT_CENTS {
        let (mut mid_power, mut side_power) = (0.0, 0.0);
        for chunk in samples.chunks(8_192) {
            if !keep_going() {
                return false;
            }
            for frame in chunk.as_chunks::<2>().0 {
                let (left, right) = (f64::from(frame[0]), f64::from(frame[1]));
                mid_power += (left + right).powi(2);
                side_power += (left - right).powi(2);
            }
        }
        let right_sign = if side_power > 4.0 * mid_power {
            -1.0
        } else {
            1.0
        };
        let mono = samples
            .as_chunks::<2>()
            .0
            .iter()
            .map(|frame| 0.5 * (frame[0] + right_sign * frame[1]));
        let Some(track) = analyse_while(mono, sample_rate, &mut keep_going) else {
            return false;
        };
        if track.voiced.is_empty() {
            return true;
        }
        Some(track)
    } else {
        None
    };
    let segments: Vec<_> = (0..frames)
        .step_by(hop)
        .take_while(|&start| start == 0 || start + overlap < frames)
        .map(|start| ShiftSegment {
            start,
            end: (start + hop + overlap).min(frames),
            cents: 0.0,
            formant_cents: cents,
        })
        .collect();
    reshape_envelope(
        samples,
        2,
        &segments,
        &config,
        overlap,
        voice_track.as_ref(),
        keep_going,
    )
    .is_some()
}

/// Shift `input` (interleaved, `channels` = 1 or 2) by `curve`.
///
/// Returns audio of exactly the same length. A curve with no non-zero segment
/// returns the input unchanged, bit for bit.
pub fn shift(
    input: &[f32],
    channels: usize,
    sample_rate: u32,
    curve: &ShiftCurve,
    config: &ShiftConfig,
) -> Result<ShiftOutput, ShiftError> {
    if channels != 1 && channels != 2 {
        return Err(ShiftError::ChannelCount(channels));
    }
    if sample_rate == 0 {
        return Err(ShiftError::ZeroSampleRate);
    }
    if !input.len().is_multiple_of(channels) {
        return Err(ShiftError::RaggedBuffer);
    }
    if input.iter().any(|sample| !sample.is_finite()) {
        return Err(ShiftError::NonFiniteInput);
    }
    let frames = input.len() / channels;
    if curve.frames != frames {
        return Err(ShiftError::CurveLength {
            curve: curve.frames,
            frames,
        });
    }

    let mut report = ShiftReport {
        frames,
        ..ShiftReport::default()
    };
    report.input_max_step = max_step(input, channels);
    if frames == 0 || curve.is_silent() {
        report.output_max_step = report.input_max_step;
        report.peak = peak(input);
        return Ok(ShiftOutput {
            samples: input.to_vec(),
            report,
        });
    }

    let cents = curve.ramped(config.ramp_frames);
    report.shifted_frames = cents.iter().filter(|value| **value != 0.0).count();
    let formant = curve.formant_ramped(config.ramp_frames);
    report.formant_frames = formant.iter().filter(|value| **value != 0.0).count();

    // The envelope moves on the take, then the PSOLA path resynthesises the
    // pitch. Doing it in this order lets the envelope come from the clean
    // source and lets PSOLA preserve the already-warped vocal tract.
    let mut shaped = input.to_vec();
    if report.formant_frames > 0 {
        let stats = reshape_envelope(
            &mut shaped,
            channels,
            &curve.segments,
            config,
            0,
            None,
            || true,
        )
        .expect("Vocal curve rendering is not cancellable");
        report.formant_limited_frames = stats.limited_frames;
        report.formant_max_gain_db = stats.max_gain_db;
    }
    let mut output = shaped.clone();

    let mono: Vec<f32> = (0..frames)
        .map(|frame| match channels {
            1 => input[frame],
            _ => 0.5 * (input[frame * 2] + input[frame * 2 + 1]),
        })
        .collect();
    let track = analyse(&mono, sample_rate);
    report.voiced_frames = track.voiced.iter().map(|(start, end)| end - start).sum();

    let mut wet_edges: Vec<usize> = Vec::new();
    let mut marks = Vec::new();
    for (start, end) in active_runs(&cents) {
        for (voiced_start, voiced_end) in &track.voiced {
            let run_start = start.max(*voiced_start);
            let run_end = end.min(*voiced_end);
            if run_end <= run_start + 1 {
                continue;
            }
            marks.clear();
            pick_marks(&mono, &track, run_start, run_end, &mut marks);
            if marks.is_empty() {
                continue;
            }
            report.wet_frames += run_end - run_start;
            wet_edges.push(run_start);
            wet_edges.push(run_end);
            synthesise(
                &mut output,
                &shaped,
                channels,
                frames,
                &cents,
                config,
                &marks,
                run_start,
                run_end,
            );
        }
    }

    report.output_max_step = max_step(&output, channels);
    let mut boundaries = wet_edges;
    for segment in &curve.segments {
        if segment.formant_cents != 0.0 {
            boundaries.push(segment.start);
            boundaries.push(segment.end);
        }
    }
    let boundaries = boundaries;
    report.boundary_step_ratio = boundary_step_ratio(input, &output, channels, frames, &boundaries);
    report.peak = peak(&output);
    debug_assert!(output.iter().all(|sample| sample.is_finite()));
    Ok(ShiftOutput {
        samples: output,
        report,
    })
}

/// One note's timing edit: move the content of `[source_start, source_end)` to
/// `[target_start, target_end)`.
///
/// Spans are half open and in frames of the same buffer. A target may be
/// shorter or longer than its source, which is time scaling by pitch periods:
/// the pitch does not change. Sources are disjoint, and so are targets; the
/// caller is responsible for rejecting an edit that collides with a note it did
/// not move, and the pass only checks the list it is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeEdit {
    pub source_start: usize,
    pub source_end: usize,
    pub target_start: usize,
    pub target_end: usize,
}

impl TimeEdit {
    /// An edit that maps a source span onto a target span.
    pub fn new(source: (usize, usize), target: (usize, usize)) -> Self {
        Self {
            source_start: source.0,
            source_end: source.1,
            target_start: target.0,
            target_end: target.1,
        }
    }

    /// True when the edit would put the content back where it already is.
    pub fn is_noop(&self) -> bool {
        self.source_start == self.target_start && self.source_end == self.target_end
    }

    fn source(&self) -> (usize, usize) {
        (self.source_start, self.source_end)
    }

    fn target(&self) -> (usize, usize) {
        (self.target_start, self.target_end)
    }
}

/// Fades for one [`time_warp`] call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeConfig {
    /// Crossfade at the destination edges, in frames (default 2 ms).
    pub edge_fade_frames: usize,
    /// Crossfade where an erased span meets the take's room, in frames.
    pub fill_fade_frames: usize,
}

impl TimeConfig {
    /// Default fades for `sample_rate`, rounded to whole frames.
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        Self {
            edge_fade_frames: (rate * TIME_EDGE_FADE_S).round().max(1.0) as usize,
            fill_fade_frames: (rate * TIME_FILL_FADE_S).round().max(1.0) as usize,
        }
    }
}

impl Default for TimeConfig {
    fn default() -> Self {
        Self::new(48_000)
    }
}

/// What the time pass did.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TimeReport {
    pub frames: usize,
    /// Edits that moved audio (exact no-ops are dropped).
    pub notes: usize,
    /// Frames written inside a destination span.
    pub edited_frames: usize,
    /// Frames erased from a source span and filled with the take's room.
    pub room_frames: usize,
    /// Largest output sample step, all channels.
    pub output_max_step: f32,
    /// Largest input sample step, all channels.
    pub input_max_step: f32,
    /// Worst step within 20 ms of a seam over the largest input step in that
    /// window or inside the edited note's own span. A fade keeps it near one;
    /// a hard splice that lands on unrelated material pushes it up.
    pub boundary_step_ratio: f32,
    /// Largest absolute output sample.
    pub peak: f32,
}

/// Warped buffer plus its report.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeOutput {
    pub samples: Vec<f32>,
    pub report: TimeReport,
}

/// Time pass for note timing: move and stretch spans inside a fixed buffer.
///
/// The length never changes. Each edited note is resynthesised with grains one
/// source period apart, so the pitch is untouched and only the number of
/// periods changes; the vacated part of the source span is filled from the
/// take's own room (the quietest free window of the same take), never from
/// synthesised noise. An empty or all-no-op edit list returns `input` unchanged,
/// bit for bit.
pub fn time_warp(
    input: &[f32],
    channels: usize,
    sample_rate: u32,
    edits: &[TimeEdit],
    config: &TimeConfig,
) -> Result<TimeOutput, ShiftError> {
    if channels != 1 && channels != 2 {
        return Err(ShiftError::ChannelCount(channels));
    }
    if sample_rate == 0 {
        return Err(ShiftError::ZeroSampleRate);
    }
    if !input.len().is_multiple_of(channels) {
        return Err(ShiftError::RaggedBuffer);
    }
    if input.iter().any(|sample| !sample.is_finite()) {
        return Err(ShiftError::NonFiniteInput);
    }
    let frames = input.len() / channels;
    let mut report = TimeReport {
        frames,
        input_max_step: max_step(input, channels),
        ..TimeReport::default()
    };

    let mut ordered = Vec::with_capacity(edits.len());
    for edit in edits {
        if edit.source_start >= edit.source_end || edit.target_start >= edit.target_end {
            return Err(ShiftError::EmptyEdit);
        }
        if edit.source_end > frames || edit.target_end > frames {
            return Err(ShiftError::EditOutOfRange);
        }
        ordered.push(*edit);
    }
    ordered.sort_by_key(|edit| edit.source_start);
    for pair in ordered.windows(2) {
        if pair[0].source_end > pair[1].source_start {
            return Err(ShiftError::OverlappingEdits);
        }
    }
    let mut targets: Vec<(usize, usize)> = ordered.iter().map(TimeEdit::target).collect();
    targets.sort_unstable();
    for pair in targets.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(ShiftError::OverlappingEdits);
        }
    }
    let active: Vec<TimeEdit> = ordered.iter().copied().filter(|e| !e.is_noop()).collect();
    if active.is_empty() {
        report.output_max_step = report.input_max_step;
        report.peak = peak(input);
        return Ok(TimeOutput {
            samples: input.to_vec(),
            report,
        });
    }

    let mono: Vec<f32> = (0..frames)
        .map(|frame| match channels {
            1 => input[frame],
            _ => 0.5 * (input[frame * 2] + input[frame * 2 + 1]),
        })
        .collect();
    let track = analyse(&mono, sample_rate);
    let mut output = input.to_vec();

    // The fill never reads a note: every edited span, every destination and
    // every detected voiced run is protected.
    let mut protected: Vec<(usize, usize)> =
        Vec::with_capacity(ordered.len() * 2 + track.voiced.len());
    for edit in &ordered {
        protected.push(edit.source());
        protected.push(edit.target());
    }
    protected.extend(track.voiced.iter().copied());
    let protected = merge_spans(protected, frames);
    let free = free_spans(frames, &protected);

    // Lift each edited note out of the take: its whole source span becomes the
    // take's room, and the destination is then written over it. A destination
    // therefore never fades in from the moved note's own old waveform.
    let fills: Vec<(usize, usize)> = active.iter().map(TimeEdit::source).collect();
    let min_room = (f64::from(sample_rate) * TIME_MIN_ROOM_S).round().max(8.0) as usize;
    // A fill edge only fades toward material the pass leaves alone. An edge
    // that another edit overwrites or erases has nothing to be continuous with.
    let mut moving = targets.clone();
    moving.extend(active.iter().map(TimeEdit::source));
    let moving = merge_spans(moving, frames);
    let untouched = |position: usize| {
        !moving
            .iter()
            .any(|(start, end)| position >= *start && position < *end)
    };
    for span in &fills {
        report.room_frames += span.1 - span.0;
        let left_open = span.0 == 0 || untouched(span.0 - 1);
        let right_open = span.1 >= frames || untouched(span.1);
        fill_gap(
            &mut output,
            input,
            channels,
            frames,
            *span,
            &free,
            min_room,
            config.fill_fade_frames,
            (left_open, right_open),
        );
    }

    let mut seams: Vec<(usize, (usize, usize))> = Vec::new();
    for edit in &active {
        for mark in [
            edit.source_start,
            edit.source_end,
            edit.target_start,
            edit.target_end,
        ] {
            seams.push((mark, edit.source()));
        }
        let mut marks = Vec::new();
        pick_marks(
            &mono,
            &track,
            edit.source_start,
            edit.source_end,
            &mut marks,
        );
        if marks.is_empty() {
            // Unvoiced content has no pitch marks: resample it instead, so a
            // moved breath still moves (its pitch is not a thing to preserve).
            resample_span(
                &mut output,
                input,
                channels,
                frames,
                edit.source(),
                edit.target(),
                config.edge_fade_frames,
            );
        } else {
            synthesise_time(
                &mut output,
                input,
                channels,
                frames,
                &marks,
                TimeSynthesisRegion {
                    source: edit.source(),
                    target: edit.target(),
                    edge_fade: config.edge_fade_frames,
                },
            );
        }
        report.edited_frames += edit.target_end - edit.target_start;
    }

    report.notes = active.len();
    report.output_max_step = max_step(&output, channels);
    report.boundary_step_ratio = seam_step_ratio(input, &output, channels, frames, &seams);
    report.peak = peak(&output);
    debug_assert!(output.iter().all(|sample| sample.is_finite()));
    Ok(TimeOutput {
        samples: output,
        report,
    })
}

struct TimeSynthesisRegion {
    source: (usize, usize),
    target: (usize, usize),
    edge_fade: usize,
}

/// Resynthesise one source span into its destination by repeating or dropping
/// periods: the epoch grid sits in the destination at the source period, and
/// the analysis mark it reads is chosen by the inverse of the linear time map.
fn synthesise_time(
    output: &mut [f32],
    input: &[f32],
    channels: usize,
    frames: usize,
    marks: &[Mark],
    region: TimeSynthesisRegion,
) {
    let TimeSynthesisRegion {
        source,
        target,
        edge_fade,
    } = region;
    let source_len = (source.1 - source.0).max(1) as f64;
    let target_len = (target.1 - target.0) as f64;
    if target_len <= 0.0 {
        return;
    }
    let slope = target_len / source_len;
    let longest = marks
        .iter()
        .fold(MIN_PERIOD, |worst, mark| worst.max(mark.period));
    let pad = psola::grain_radius(longest, longest, 1.0) + 2;
    let base = target.0.saturating_sub(pad);
    let stop = (target.1 + pad).min(frames);
    if stop <= base {
        return;
    }
    let span = stop - base;
    let mut wet_l = vec![0.0_f64; span];
    let mut wet_r = vec![0.0_f64; span];
    let mut weight = vec![0.0_f64; span];

    let mut index = 0_usize;
    let mut epoch = target.0 as f64 + (marks[0].pos - source.0 as f64) * slope;
    let limit = (target_len / 2.0) as usize + 8;
    let mut placed = 0_usize;
    loop {
        let source_pos = source.0 as f64 + (epoch - target.0 as f64) / slope;
        while index + 1 < marks.len()
            && (marks[index + 1].pos - source_pos).abs() <= (marks[index].pos - source_pos).abs()
        {
            index += 1;
        }
        let mark = marks[index];
        let period = mark.period.max(MIN_PERIOD);
        let radius = psola::grain_radius(period, period, 1.0);
        if epoch - radius as f64 > target.1 as f64 {
            break;
        }
        accumulate(
            &mut wet_l,
            &mut wet_r,
            &mut weight,
            input,
            channels,
            frames,
            base,
            span,
            mark.pos,
            epoch,
            radius,
        );
        epoch += period;
        placed += 1;
        // The chain ends with the note's own content. Repeating the last period
        // over the analyser's silent tail would sustain a note past its end.
        if placed > limit || index + 1 >= marks.len() {
            break;
        }
    }

    let width = target.1 - target.0;
    let fade = edge_fade.min(width / 2);
    // Period add/drop keeps every grain at the source level, so unlike the
    // pitch path there is no cancellation to trim against the room underneath.
    let trim_ramp = fade.max(1).min(width / 2).max(1);
    blend_run(
        output, &wet_l, &wet_r, &weight, channels, base, target.0, width, fade, 1.0, trim_ramp,
    );
}

/// Fallback for a span with no pitch marks: linear resample at unity gain.
#[allow(clippy::too_many_arguments)]
fn resample_span(
    output: &mut [f32],
    input: &[f32],
    channels: usize,
    frames: usize,
    source: (usize, usize),
    target: (usize, usize),
    edge_fade: usize,
) {
    let source_len = (source.1 - source.0).max(1) as f64;
    let width = target.1 - target.0;
    if width == 0 {
        return;
    }
    let fade = edge_fade.min(width / 2);
    let pre: Vec<f64> = if target.0 > 0 {
        (0..channels)
            .map(|channel| f64::from(input[(target.0 - 1) * channels + channel]))
            .collect()
    } else {
        vec![0.0; channels]
    };
    let post: Vec<f64> = if target.1 < frames {
        (0..channels)
            .map(|channel| f64::from(input[target.1 * channels + channel]))
            .collect()
    } else {
        vec![0.0; channels]
    };
    for offset in 0..width {
        let position = source.0 as f64 + offset as f64 * source_len / width as f64;
        let index = position.floor() as usize;
        let fraction = position - index as f64;
        let left = (index * channels).min(input.len().saturating_sub(channels));
        let right = ((index + 1) * channels).min(input.len().saturating_sub(channels));
        let weight = if fade > 0 && offset < fade {
            ramp_weight(offset, fade)
        } else if fade > 0 && offset + fade >= width {
            ramp_weight(width - offset - 1, fade)
        } else {
            1.0
        };
        let dry = if offset * 2 < width { &pre } else { &post };
        for channel in 0..channels {
            let a = f64::from(input[left + channel]);
            let b = f64::from(input[right + channel]);
            let wet = a + (b - a) * fraction;
            let value = wet * weight + dry[channel] * (1.0 - weight);
            output[(target.0 + offset) * channels + channel] = value as f32;
        }
    }
}

/// Fill `span` with the take's own room, crossfaded into its neighbours.
///
/// The room comes from the free gap closest to the span that is long enough;
/// when only a shorter one exists, that much is copied and the rest is faded to
/// silence (the take has no more room to give). Nothing is synthesised.
#[allow(clippy::too_many_arguments)]
fn fill_gap(
    output: &mut [f32],
    pristine: &[f32],
    channels: usize,
    frames: usize,
    span: (usize, usize),
    free: &[(usize, usize)],
    min_room: usize,
    fade: usize,
    open: (bool, bool),
) {
    let needed = span.1 - span.0;
    if needed == 0 {
        return;
    }
    let distance = |gap: (usize, usize)| {
        if gap.1 <= span.0 {
            span.0 - gap.1
        } else {
            gap.0.saturating_sub(span.1)
        }
    };
    let candidates: Vec<(usize, usize)> = free
        .iter()
        .copied()
        .filter(|(start, end)| end - start >= min_room)
        .collect();
    let chosen = candidates
        .iter()
        .copied()
        .filter(|(start, end)| end - start >= needed)
        .min_by_key(|gap| distance(*gap))
        .or_else(|| {
            candidates
                .iter()
                .copied()
                .max_by_key(|(start, end)| end - start)
        });
    let (copy_len, source_start) = match chosen {
        Some((start, end)) if end - start >= needed => {
            let source_start = if end <= span.0 { end - needed } else { start };
            (needed, source_start)
        }
        Some((start, end)) => {
            let copy_len = end - start;
            let source_start = if end <= span.0 { end - copy_len } else { start };
            (copy_len, source_start)
        }
        None => (0, 0),
    };
    let fade = fade.min(needed / 2);
    let pre: Vec<f64> = if span.0 > 0 {
        (0..channels)
            .map(|channel| f64::from(pristine[(span.0 - 1) * channels + channel]))
            .collect()
    } else {
        vec![0.0; channels]
    };
    let post: Vec<f64> = if span.1 < frames {
        (0..channels)
            .map(|channel| f64::from(pristine[span.1 * channels + channel]))
            .collect()
    } else {
        vec![0.0; channels]
    };
    for offset in 0..needed {
        let left = if open.0 && fade > 0 && offset < fade {
            ramp_weight(offset, fade)
        } else {
            1.0
        };
        let right = if open.1 && fade > 0 && offset + fade >= needed {
            ramp_weight(needed - offset - 1, fade)
        } else {
            1.0
        };
        let weight = left.min(right);
        let dry = if offset * 2 < needed { &pre } else { &post };
        for channel in 0..channels {
            let room = if offset < copy_len {
                f64::from(pristine[(source_start + offset) * channels + channel])
            } else {
                0.0
            };
            let value = room * weight + dry[channel] * (1.0 - weight);
            output[(span.0 + offset) * channels + channel] = value as f32;
        }
    }
}

/// Merge spans into sorted, disjoint intervals clipped to `frames`.
fn merge_spans(mut spans: Vec<(usize, usize)>, frames: usize) -> Vec<(usize, usize)> {
    spans.retain(|(start, end)| start < end);
    spans.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (start, end) in spans {
        let (start, end) = (start.min(frames), end.min(frames));
        if start >= end {
            continue;
        }
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// Maximal spans of `[0, frames)` outside `spans` (sorted, disjoint).
fn free_spans(frames: usize, spans: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut cursor = 0;
    for &(start, end) in spans {
        if start > cursor {
            out.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if cursor < frames {
        out.push((cursor, frames));
    }
    out
}

/// Worst step within 20 ms of a seam over the largest input step in that
/// window or inside the seam's own source span.
fn seam_step_ratio(
    input: &[f32],
    output: &[f32],
    channels: usize,
    frames: usize,
    seams: &[(usize, (usize, usize))],
) -> f32 {
    let window = (frames / 50).max(8);
    let mut worst = 0.0_f32;
    for (edge, source) in seams {
        let low = edge.saturating_sub(window);
        let high = (edge + window).min(frames);
        let was = step_peak(input, channels, low, high)
            .max(step_peak(input, channels, source.0, source.1));
        let now = step_peak(output, channels, low, high);
        if was > 1e-9 {
            worst = worst.max(now / was);
        }
    }
    worst
}

/// One analysis pitch mark: position in frames and the period that starts there.
#[derive(Debug, Clone, Copy)]
struct Mark {
    pos: f64,
    period: f64,
}

/// Period track plus the sample spans the detector called voiced.
#[derive(Debug, Clone, Default)]
struct Track {
    sample_rate: u32,
    frames: Vec<(f64, f64)>,
    voiced: Vec<(usize, usize)>,
}

impl Track {
    fn period_at(&self, position: f64) -> f64 {
        if self.frames.is_empty() {
            return MIN_PERIOD;
        }
        let index = self
            .frames
            .partition_point(|(centre, _)| *centre < position)
            .min(self.frames.len() - 1);
        self.frames[index].1.max(MIN_PERIOD)
    }
}

/// Decimate, run YIN every hop and smooth with the realtime period tracker.
fn analyse(mono: &[f32], sample_rate: u32) -> Track {
    analyse_while(mono.iter().copied(), sample_rate, || true).expect("uninterrupted analysis")
}

fn analyse_while(
    mono: impl IntoIterator<Item = f32>,
    sample_rate: u32,
    mut keep_going: impl FnMut() -> bool,
) -> Option<Track> {
    let mut decimator = Decimator::new(sample_rate);
    let factor = decimator.factor() as f64;
    let rate = decimator.rate();
    let mut detector = YinDetector::new(rate, FULL_WINDOW_S, MIN_F0);
    let mut tracker = PeriodTracker::default();
    let hop = (rate * ANALYSIS_HOP_S).round().max(1.0) as usize;
    let span = detector.window() + detector.tau_max() + 1;
    let half_hop = hop as f64 * factor * 0.5;
    let mut track = Track {
        sample_rate,
        ..Track::default()
    };
    let mut centres: Vec<(f64, bool)> = Vec::new();
    let mut pushed = 0_usize;
    for sample in mono {
        let Some(value) = decimator.push(f64::from(sample)) else {
            continue;
        };
        detector.push(value);
        pushed += 1;
        if !pushed.is_multiple_of(hop) {
            continue;
        }
        if !keep_going() {
            return None;
        }
        tracker.update(detector.detect(), rate);
        let centre = (pushed as f64 - span as f64 + detector.window() as f64 * 0.5) * factor;
        let voiced = tracker.valid && (MIN_F0..=MAX_F0).contains(&tracker.f0);
        if voiced {
            track.frames.push((centre, tracker.period * factor));
        }
        centres.push((centre, voiced));
    }
    // Close single-frame dropouts so one noisy frame cannot split a note.
    let mut current: Option<(usize, usize)> = None;
    let mut gap = 0_usize;
    for (centre, voiced) in centres {
        let start = (centre - half_hop).max(0.0) as usize;
        let end = (centre + half_hop).max(0.0) as usize;
        if voiced {
            gap = 0;
            current = Some(match current {
                Some((first, _)) => (first, end),
                None => (start, end),
            });
        } else if let Some((first, last)) = current {
            gap += 1;
            if gap > BRIDGE_FRAMES {
                track.voiced.push((first, last));
                current = None;
                gap = 0;
            }
        }
    }
    if let Some(interval) = current {
        track.voiced.push(interval);
    }
    Some(track)
}

/// Maximal spans where the ramp made the curve non-zero.
fn active_runs(cents: &[f64]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut index = 0;
    while index < cents.len() {
        if cents[index] == 0.0 {
            index += 1;
            continue;
        }
        let start = index;
        while index < cents.len() && cents[index] != 0.0 {
            index += 1;
        }
        runs.push((start, index));
    }
    runs
}

/// Five-tap smoothing of the mono mix, the same peak preconditioner the
/// realtime picker uses.
fn intensity(mono: &[f32], index: usize) -> f64 {
    let mut sum = 0.0;
    for back in 0..5 {
        let slot = index.saturating_sub(back);
        sum += f64::from(mono[slot]);
    }
    sum * 0.2
}

/// Parabolic-refined local maximum of the smoothed signal inside a window.
fn peak_near(mono: &[f32], predicted: f64, half: f64, low: usize, high: usize) -> Option<f64> {
    let from = ((predicted - half).ceil().max(low as f64) as usize).max(4);
    let to = (predicted + half)
        .floor()
        .min((high.saturating_sub(1)) as f64) as usize;
    if to <= from {
        return None;
    }
    let mut best = from;
    let mut value = f64::NEG_INFINITY;
    for index in from..=to {
        let current = intensity(mono, index);
        if current > value {
            value = current;
            best = index;
        }
    }
    if value < SILENCE_PEAK {
        return None;
    }
    let offset =
        psola::parabolic_offset(intensity(mono, best - 1), value, intensity(mono, best + 1));
    Some(best as f64 + offset)
}

/// First sample of `start..end` that carries signal.
///
/// A detected span can begin a little before the first audible sample (the
/// detector frame is centred), and a run whose chain starts in that silence
/// would otherwise be dropped whole.
fn first_audible(mono: &[f32], start: usize, end: usize) -> Option<usize> {
    let mut index = start;
    while index + 4 < end {
        let level = (0..4).fold(0.0_f64, |peak, offset| {
            peak.max(f64::from(mono[index + offset]).abs())
        });
        if level >= SILENCE_PEAK {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// Walk a pitch-mark chain across one voiced span.
fn pick_marks(mono: &[f32], track: &Track, start: usize, end: usize, out: &mut Vec<Mark>) {
    let Some(start) = first_audible(mono, start, end) else {
        return;
    };
    if end < start + 8 {
        return;
    }
    let period = track.period_at(start as f64);
    let Some(mut position) = peak_near(mono, start as f64 + period * 0.5, period, start, end)
    else {
        return;
    };
    let mut last_period = period;
    loop {
        let local = track.period_at(position);
        let Some(next) = peak_near(mono, position + local, local * MARK_WINDOW, start, end) else {
            break;
        };
        if next <= position + 1.0 || next >= (end - 1) as f64 {
            break;
        }
        last_period = next - position;
        out.push(Mark {
            pos: position,
            period: last_period,
        });
        position = next;
    }
    if out.is_empty() {
        return;
    }
    out.push(Mark {
        pos: position,
        period: last_period.max(MIN_PERIOD),
    });
}

/// Resynthesise one wet run into `output`.
#[allow(clippy::too_many_arguments)]
fn synthesise(
    output: &mut [f32],
    input: &[f32],
    channels: usize,
    frames: usize,
    cents: &[f64],
    config: &ShiftConfig,
    marks: &[Mark],
    run_start: usize,
    run_end: usize,
) {
    let scale = f64::from(config.grain_scale.max(0.25));
    let longest = marks
        .iter()
        .fold(MIN_PERIOD, |worst, mark| worst.max(mark.period));
    let pad = psola::grain_radius(longest, longest * 2.0, scale) + 2;
    let base = run_start.saturating_sub(pad);
    let stop = (run_end + pad).min(frames);
    if stop <= base {
        return;
    }
    let span = stop - base;
    let mut wet_l = vec![0.0_f64; span];
    let mut wet_r = vec![0.0_f64; span];
    let mut weight = vec![0.0_f64; span];
    let reference = if config.preserve_period_contour {
        0.0
    } else {
        let mut periods: Vec<f64> = marks.iter().map(|mark| mark.period).collect();
        periods.sort_by(|left, right| left.total_cmp(right));
        periods[periods.len() / 2]
    };

    let mut index = 0_usize;
    while index + 1 < marks.len() && marks[index].pos < run_start as f64 - marks[index].period * 0.5
    {
        index += 1;
    }
    let mut epoch = marks[index].pos;
    let limit = (run_end - base) * 2 + 8;
    let mut placed = 0_usize;
    loop {
        while index + 1 < marks.len()
            && (marks[index + 1].pos - epoch).abs() <= (marks[index].pos - epoch).abs()
        {
            index += 1;
        }
        let mark = marks[index];
        let source = if reference > 0.0 {
            reference
        } else {
            mark.period
        };
        let ratio =
            psola::cents_ratio(cents[epoch.round().clamp(0.0, (frames - 1) as f64) as usize]);
        let period_out = psola::synth_hop(source, ratio);
        let radius = psola::grain_radius(mark.period, period_out, scale);
        if epoch - radius as f64 > stop as f64 {
            break;
        }
        accumulate(
            &mut wet_l,
            &mut wet_r,
            &mut weight,
            input,
            channels,
            frames,
            base,
            span,
            mark.pos,
            epoch,
            radius,
        );
        epoch += period_out;
        placed += 1;
        if placed > limit || index + 1 >= marks.len() {
            break;
        }
    }

    let width = (run_end - run_start).min(span);
    let fade = config
        .crossfade_frames
        .min(width / 2)
        .min(config.ramp_frames.max(1) / 2);
    // Duplicated grains cancel for large lifts, so trim each run back to the
    // level it replaces. The trim ramps in over the slew so it cannot click.
    let trim = run_trim(
        output, &wet_l, &wet_r, &weight, channels, run_start, base, width,
    );
    let trim_ramp = config.ramp_frames.max(1).min(width / 2).max(1);
    blend_run(
        output, &wet_l, &wet_r, &weight, channels, base, run_start, width, fade, trim, trim_ramp,
    );
}

/// Blend one resynthesised run into `output`, window-sum normalised.
///
/// The wet signal only replaces the dry where a grain covers it; partial cover
/// keeps the dry signal as fill instead of normalising a half-covered edge up to
/// full level. `trim` restores the level the run replaces and ramps in over
/// `trim_ramp` samples so it cannot click.
#[allow(clippy::too_many_arguments)]
fn blend_run(
    output: &mut [f32],
    wet_l: &[f64],
    wet_r: &[f64],
    weight: &[f64],
    channels: usize,
    base: usize,
    run_start: usize,
    width: usize,
    fade: usize,
    trim: f64,
    trim_ramp: usize,
) {
    for offset in 0..width {
        let frame = run_start + offset;
        let slot = frame - base;
        let covered = weight[slot];
        if covered <= WEIGHT_FLOOR {
            continue;
        }
        let fade_gain = if fade == 0 {
            1.0
        } else if offset < fade {
            ramp_weight(offset, fade)
        } else if offset + fade >= width {
            ramp_weight(width - offset - 1, fade)
        } else {
            1.0
        };
        let fill = 1.0 - fade_gain * covered.min(1.0);
        let normalise = if covered > 1.0 { 1.0 / covered } else { 1.0 };
        let edge = offset.min(width - 1 - offset);
        let trim_gain = 1.0 + (trim - 1.0) * (edge as f64 / trim_ramp as f64).min(1.0);
        let wet = [
            wet_l[slot] * normalise * trim_gain,
            wet_r[slot] * normalise * trim_gain,
        ];
        for channel in 0..channels {
            let dry = f64::from(output[frame * channels + channel]);
            let blended = dry * fill + wet[channel] * fade_gain;
            output[frame * channels + channel] = blended as f32;
        }
    }
}

/// Level ratio that puts the fully covered wet back at the dry level.
#[allow(clippy::too_many_arguments)]
fn run_trim(
    output: &[f32],
    wet_l: &[f64],
    wet_r: &[f64],
    weight: &[f64],
    channels: usize,
    run_start: usize,
    base: usize,
    width: usize,
) -> f64 {
    let mut dry = 0.0;
    let mut wet = 0.0;
    let mut counted = 0_usize;
    for offset in 0..width {
        let slot = run_start + offset - base;
        if weight[slot] < 0.999 {
            continue;
        }
        let frame = run_start + offset;
        let left = wet_l[slot] / weight[slot];
        let right = wet_r[slot] / weight[slot];
        wet += left * left + right * right;
        for channel in 0..channels {
            let value = f64::from(output[frame * channels + channel]);
            dry += value * value;
        }
        counted += 1;
    }
    if counted < 8 || wet <= 1e-18 || dry <= 1e-18 {
        return 1.0;
    }
    (dry / wet).sqrt().clamp(0.5, 2.0)
}

/// Add one grain to the wet accumulator.
#[allow(clippy::too_many_arguments)]
fn accumulate(
    wet_l: &mut [f64],
    wet_r: &mut [f64],
    weight: &mut [f64],
    input: &[f32],
    channels: usize,
    frames: usize,
    base: usize,
    span: usize,
    source_centre: f64,
    epoch: f64,
    radius: usize,
) {
    let first = (epoch - radius as f64).ceil().max(base as f64) as usize;
    let last = ((epoch + radius as f64).floor() as usize).min(base + span - 1);
    let start = epoch - radius as f64;
    for frame in first..=last {
        let window = psola::grain_window(frame as f64 - start, radius);
        if window <= 0.0 {
            continue;
        }
        let source = source_centre + (frame as f64 - epoch);
        if source < 1.0 || source > (frames - 3) as f64 {
            continue;
        }
        let index = source.floor() as usize;
        let [a, b, c, d] = lagrange(source - index as f64);
        let slot = frame - base;
        let first_tap = (index - 1) * channels;
        for channel in 0..channels {
            let value = a * f64::from(input[first_tap + channel])
                + b * f64::from(input[first_tap + channels + channel])
                + c * f64::from(input[first_tap + 2 * channels + channel])
                + d * f64::from(input[first_tap + 3 * channels + channel]);
            if channel == 0 {
                wet_l[slot] += window * value;
            } else {
                wet_r[slot] += window * value;
            }
        }
        weight[slot] += window;
    }
}

/// Four-point Lagrange weights for a read at `fraction` between the samples at
/// index and index + 1.
fn lagrange(fraction: f64) -> [f64; 4] {
    let outer = -fraction * (fraction - 1.0) * (fraction - 2.0) / 6.0;
    let left = (fraction + 1.0) * (fraction - 1.0) * (fraction - 2.0) / 2.0;
    let right = -(fraction + 1.0) * fraction * (fraction - 2.0) / 2.0;
    let far = (fraction + 1.0) * fraction * (fraction - 1.0) / 6.0;
    [outer, left, right, far]
}

/// What the envelope pass did.
#[derive(Debug, Clone, Copy, Default)]
struct FormantStats {
    limited_frames: usize,
    max_gain_db: f32,
}

/// One segment's envelope correction, designed before any sample moves.
struct FormantPlan {
    start: usize,
    end: usize,
    /// Zero-phase FIR kernel, length `n`.
    kernel: Vec<f64>,
    limited: bool,
    max_gain_db: f32,
}

/// Move each segment's spectral envelope by its formant ratio.
///
/// Per segment: estimate the cepstral envelope on every analysis frame inside
/// the note and average the log envelopes, read the average at `f / ratio`,
/// and turn the log ratio into a zero-phase FIR. The FIR is applied with
/// overlap-add block convolution. Applying one filter per note is deliberate:
/// a per-frame gain is a linear periodically time-varying system whose hop-rate
/// modulation adds sidebands (measured at -3 dBc on a periodic vowel), while a
/// constant filter leaves a periodic excitation periodic (measured -70 dBc).
/// Samples outside the segment's edges are only read as filter context.
/// Cepstral envelope estimator: one FFT plan set and scratch buffers reused
/// for every segment of one call.
struct EnvelopeEstimator {
    n: usize,
    hop: usize,
    lifter: usize,
    side_fallback: bool,
    window: Vec<f64>,
    mono: Vec<Complex<f64>>,
    log_full: Vec<f64>,
    cepstrum: Vec<Complex<f64>>,
    forward: std::sync::Arc<dyn rustfft::Fft<f64>>,
    inverse: std::sync::Arc<dyn rustfft::Fft<f64>>,
}

impl EnvelopeEstimator {
    fn new(n: usize, hop: usize, lifter: usize, side_fallback: bool) -> Self {
        let mut planner = FftPlanner::<f64>::new();
        Self {
            n,
            hop,
            lifter,
            side_fallback,
            window: (0..n)
                .map(|index| (0.5 - 0.5 * (2.0 * PI * index as f64 / n as f64).cos()).sqrt())
                .collect(),
            mono: vec![Complex::new(0.0, 0.0); n],
            log_full: vec![0.0_f64; n],
            cepstrum: vec![Complex::new(0.0, 0.0); n],
            forward: planner.plan_fft_forward(n),
            inverse: planner.plan_fft_inverse(n),
        }
    }

    /// Mean log envelope over the frames whose centre falls in `[start, end)`.
    fn estimate(
        &mut self,
        samples: &[f32],
        channels: usize,
        frames: usize,
        start: usize,
        end: usize,
        voice_track: Option<&Track>,
    ) -> Option<Vec<f64>> {
        let n = self.n;
        let hop = self.hop;
        let bins = n / 2 + 1;
        let floor_ratio = 10f64.powf(FORMANT_ENVELOPE_FLOOR_DB / 20.0);
        let mut envelope = vec![0.0_f64; bins];
        let mut counted = 0_usize;
        let mut frame_start = (start.saturating_sub(n / 2) / hop) * hop;
        while frame_start <= end {
            let centre = frame_start + n / 2;
            let outside_voiced_support = voice_track.is_some_and(|track| {
                let spans = &track.voiced;
                let first = spans.partition_point(|&(_, end)| end <= frame_start);
                spans
                    .get(first)
                    .is_none_or(|&(start, end)| frame_start < start || frame_start + n > end)
            });
            if centre < start || centre >= end || outside_voiced_support {
                frame_start += hop;
                continue;
            }
            let mut peak = 0.0_f64;
            let mut mid_power = 0.0_f64;
            let mut side_power = 0.0_f64;
            for index in 0..n {
                let position = frame_start + index;
                let value = if position < frames {
                    match channels {
                        1 => f64::from(samples[position]),
                        _ => {
                            let left = f64::from(samples[position * 2]);
                            let right = f64::from(samples[position * 2 + 1]);
                            let mid = 0.5 * (left + right);
                            if self.side_fallback {
                                let weight = self.window[index];
                                mid_power += (mid * weight).powi(2);
                                side_power += (0.5 * (left - right) * weight).powi(2);
                            }
                            mid
                        }
                    }
                } else {
                    0.0
                };
                peak = peak.max(value.abs());
                self.mono[index] = Complex::new(value * self.window[index], 0.0);
            }
            // A side-dominant clip may disappear in a mono sum. Estimate
            // from its difference signal and apply one filter to both channels.
            if side_power > 4.0 * mid_power {
                peak = 0.0;
                for (index, bin) in self.mono.iter_mut().enumerate() {
                    let position = frame_start + index;
                    let side = if position < frames {
                        0.5 * (f64::from(samples[position * 2])
                            - f64::from(samples[position * 2 + 1]))
                    } else {
                        0.0
                    };
                    peak = peak.max(side.abs());
                    *bin = Complex::new(side * self.window[index], 0.0);
                }
            }
            if peak >= FORMANT_SILENCE_PEAK {
                self.forward.process(&mut self.mono);
                let frame_peak = self
                    .mono
                    .iter()
                    .take(bins)
                    .fold(0.0_f64, |worst, value| worst.max(value.norm()));
                let floor = frame_peak * floor_ratio;
                for bin in 0..bins {
                    self.log_full[bin] = self.mono[bin].norm().max(floor).ln();
                }
                for bin in 1..n / 2 {
                    self.log_full[n - bin] = self.log_full[bin];
                }
                for bin in 0..n {
                    self.cepstrum[bin] = Complex::new(self.log_full[bin], 0.0);
                }
                self.inverse.process(&mut self.cepstrum);
                for value in &mut self.cepstrum {
                    *value /= n as f64;
                }
                for bin in self.lifter + 1..n - self.lifter {
                    self.cepstrum[bin] = Complex::new(0.0, 0.0);
                }
                if let Some(track) = voice_track.filter(|track| !track.frames.is_empty()) {
                    // The excitation peak sits at the local period. Fade the
                    // lifter before it can enter the vowel envelope.
                    let period = track.period_at(centre as f64);
                    for bin in 1..=self.lifter {
                        let phase = ((bin as f64 / period - 0.55) / 0.30).clamp(0.0, 1.0);
                        let weight = 0.5 + 0.5 * (PI * phase).cos();
                        self.cepstrum[bin] *= weight;
                        self.cepstrum[n - bin] *= weight;
                    }
                }
                self.forward.process(&mut self.cepstrum);
                for (value, coefficient) in envelope[..bins].iter_mut().zip(&self.cepstrum[..bins])
                {
                    *value += coefficient.re;
                }
                counted += 1;
            }
            frame_start += hop;
        }
        if counted == 0 {
            return None;
        }
        for value in &mut envelope {
            *value /= counted as f64;
        }
        Some(envelope)
    }
}

/// Move each segment's spectral envelope by its formant ratio.
///
/// Per segment: estimate the take's cepstral envelope once from the frames
/// inside the note, read it at `f / ratio`, and turn the log ratio into a
/// zero-phase FIR. The FIR is applied with block convolution (overlap-add)
/// before the PSOLA path, so the grain reuse only has to preserve an envelope
/// that is already correct.
///
/// One filter per note is deliberate: a per-frame gain is a linear periodically
/// time-varying system whose hop-rate modulation adds sidebands (measured at
/// -3 dBc on a periodic vowel), while a constant filter leaves a periodic
/// excitation periodic (measured below -55 dBc). Samples outside the segment
/// are only read as filter context.
fn reshape_envelope(
    output: &mut [f32],
    channels: usize,
    segments: &[ShiftSegment],
    config: &ShiftConfig,
    overlap: usize,
    voice_track: Option<&Track>,
    mut keep_going: impl FnMut() -> bool,
) -> Option<FormantStats> {
    let frames = output.len() / channels;
    let voiced = voice_track.map(|track| track.voiced.as_slice());
    let n = config.formant_window_frames.max(16).next_power_of_two();
    let hop = config.formant_hop_frames.clamp(1, n);
    let lifter = config.formant_lifter_frames.clamp(2, n / 2 - 1);
    let limit_db = config.formant_gain_limit_db.max(0.0);
    let limit_ln = (10f64.powf(limit_db / 20.0)).ln();
    let bins = n / 2 + 1;
    // Clip formant regions overlap. The note-level tuner keeps its existing
    // mono estimate when processing non-overlapping note decisions.
    let mut estimator = EnvelopeEstimator::new(n, hop, lifter, overlap > 0);
    let mut plans: Vec<FormantPlan> = Vec::new();

    for segment in segments {
        if !keep_going() {
            return None;
        }
        if segment.formant_cents == 0.0 {
            continue;
        }
        let start = segment.start.min(frames);
        let end = segment.end.min(frames);
        if end <= start {
            continue;
        }
        if voiced.is_some_and(|spans| {
            let first = spans.partition_point(|&(_, last)| last <= start);
            spans.get(first).is_none_or(|&(first, _)| first >= end)
        }) {
            continue;
        }
        let ratio = psola::cents_ratio(f64::from(segment.formant_cents));
        let Some(take) = estimator.estimate(output, channels, frames, start, end, voice_track)
        else {
            continue;
        };
        let mut spectrum = vec![Complex::new(0.0, 0.0); n];
        let mut limited = false;
        let mut max_gain_db = 0.0_f32;
        // Keep the recording's upper air and bandwidth edge in wide clip shifts.
        // On low-rate audio, finish the taper before the warped read reaches Nyquist.
        let taper = voice_track.map(|track| {
            let nyquist = f64::from(track.sample_rate) * 0.5;
            let end = 10_000.0_f64.min(nyquist * ratio.min(1.0));
            (5_000.0_f64.min(end * 0.5), end, nyquist / (bins - 1) as f64)
        });
        for bin in 0..bins {
            let read = bin as f64 / ratio;
            let target = if read >= (bins - 1) as f64 {
                take[bins - 1]
            } else {
                let index = read.floor() as usize;
                let fraction = read - index as f64;
                take[index] + (take[index + 1] - take[index]) * fraction
            };
            let mut raw = target - take[bin];
            if let Some((start, end, bin_hz)) = taper {
                let fraction = ((bin as f64 * bin_hz - start) / (end - start)).clamp(0.0, 1.0);
                raw *= 0.5 * (1.0 + (PI * fraction).cos());
            }
            let log_gain = raw.clamp(-limit_ln, limit_ln);
            if raw.abs() > limit_ln {
                limited = true;
            }
            max_gain_db = max_gain_db.max((log_gain.abs() * 20.0 / 10f64.ln()) as f32);
            let gain = log_gain.exp();
            spectrum[bin] = Complex::new(gain, 0.0);
            if bin > 0 && bin < n / 2 {
                spectrum[n - bin] = Complex::new(gain, 0.0);
            }
        }
        estimator.inverse.process(&mut spectrum);
        let normalise = 1.0 / n as f64;
        // The causal form of the zero-phase kernel is the inverse transform
        // rotated by half its length.
        let kernel: Vec<f64> = (0..n)
            .map(|index| spectrum[(index + n / 2) % n].re * normalise)
            .collect();
        plans.push(FormantPlan {
            start,
            end,
            kernel,
            limited,
            max_gain_db,
        });
    }
    if plans.is_empty() {
        return Some(FormantStats::default());
    }

    let mut stats = FormantStats::default();
    let block = n;
    let fft_size = (block + 2 * n - 1).next_power_of_two();
    let mut block_planner = FftPlanner::<f64>::new();
    let block_forward = block_planner.plan_fft_forward(fft_size);
    let block_inverse = block_planner.plan_fft_inverse(fft_size);
    let mut kernel_spectrum = vec![Complex::new(0.0, 0.0); fft_size];
    let mut block_spectrum = vec![Complex::new(0.0, 0.0); fft_size];
    // The filter reads context on both sides of every block, so it must read
    // the unfiltered buffer, never samples this pass already wrote.
    let dry = output.to_vec();
    for plan in &plans {
        for (index, value) in plan.kernel.iter().enumerate() {
            kernel_spectrum[index] = Complex::new(*value, 0.0);
        }
        for value in kernel_spectrum.iter_mut().skip(plan.kernel.len()) {
            *value = Complex::new(0.0, 0.0);
        }
        block_forward.process(&mut kernel_spectrum);
        let ramp = |edge_is_outer: bool| {
            let width = if edge_is_outer || overlap == 0 {
                config.ramp_frames
            } else {
                overlap
            };
            width.min((plan.end - plan.start) / 2).max(1)
        };
        let (head_ramp, tail_ramp) = (ramp(plan.start == 0), ramp(plan.end == frames));
        let mut start = plan.start;
        while start < plan.end {
            if !keep_going() {
                return None;
            }
            let width = block.min(plan.end - start);
            block_spectrum[..fft_size].fill(Complex::new(0.0, 0.0));
            for (index, value) in block_spectrum[..block + n].iter_mut().enumerate() {
                let position = start as isize - (n / 2) as isize + index as isize;
                if position < 0 || position >= frames as isize {
                    continue;
                }
                let position = position as usize;
                let left = f64::from(dry[position * channels]);
                let right = if channels > 1 {
                    f64::from(dry[position * channels + 1])
                } else {
                    0.0
                };
                *value = Complex::new(left, right);
            }
            block_forward.process(&mut block_spectrum);
            for (value, kernel) in block_spectrum.iter_mut().zip(&kernel_spectrum) {
                *value *= kernel;
            }
            block_inverse.process(&mut block_spectrum);
            let normalise = 1.0 / fft_size as f64;
            for offset in 0..width {
                let position = start + offset;
                let mixed = block_spectrum[offset + n].re * normalise;
                let dry_sample = f64::from(dry[position * channels]);
                let head = position - plan.start;
                let tail = plan.end - 1 - position;
                let weight = if head < head_ramp {
                    ramp_weight(head, head_ramp)
                } else if tail < tail_ramp {
                    ramp_weight(tail, tail_ramp)
                } else {
                    1.0
                };
                let left_base = if overlap == 0 {
                    dry_sample
                } else {
                    f64::from(output[position * channels])
                };
                output[position * channels] = (left_base + weight * (mixed - dry_sample)) as f32;
                if channels > 1 {
                    let mixed_right = block_spectrum[offset + n].im * normalise;
                    let dry_right = f64::from(dry[position * channels + 1]);
                    let right_base = if overlap == 0 {
                        dry_right
                    } else {
                        f64::from(output[position * channels + 1])
                    };
                    output[position * channels + 1] =
                        (right_base + weight * (mixed_right - dry_right)) as f32;
                }
            }
            start += width;
        }
        if plan.limited {
            stats.limited_frames += plan.end - plan.start;
        }
        stats.max_gain_db = stats.max_gain_db.max(plan.max_gain_db);
    }
    if let Some(voiced) = voiced {
        // A centered FIR reads half a window to either side. Keep that much
        // dry at each voiced edge so convolution cannot bring neighboring
        // unvoiced samples into the audible correction.
        let mut spans = voiced.iter().copied().peekable();
        for (frame, (wet, original)) in output
            .chunks_exact_mut(channels)
            .zip(dry.chunks_exact(channels))
            .enumerate()
        {
            if frame.is_multiple_of(block) && !keep_going() {
                return None;
            }
            while spans.peek().is_some_and(|&(_, end)| frame >= end) {
                spans.next();
            }
            match spans.peek().filter(|&&(start, _)| frame >= start) {
                Some(&(start, end)) => {
                    let edge = (frame - start).min(end - 1 - frame);
                    if edge < n / 2 {
                        wet.copy_from_slice(original);
                    } else if edge - n / 2 < config.ramp_frames {
                        let weight = ramp_weight(edge - n / 2, config.ramp_frames) as f32;
                        for (sample, &dry) in wet.iter_mut().zip(original) {
                            *sample = dry + weight * (*sample - dry);
                        }
                    }
                }
                None => wet.copy_from_slice(original),
            }
        }
    }
    Some(stats)
}

fn max_step(samples: &[f32], channels: usize) -> f32 {
    let mut worst = 0.0_f32;
    for channel in 0..channels {
        let mut previous = samples.get(channel).copied();
        for frame in 1..samples.len() / channels {
            let current = samples[frame * channels + channel];
            if let Some(before) = previous {
                worst = worst.max((current - before).abs());
            }
            previous = Some(current);
        }
    }
    worst
}

fn peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0_f32, |worst, sample| worst.max(sample.abs()))
}

/// Worst boundary step over the input's own step in the same 20 ms window.
fn boundary_step_ratio(
    input: &[f32],
    output: &[f32],
    channels: usize,
    frames: usize,
    boundaries: &[usize],
) -> f32 {
    let window = (frames / 50).max(8);
    let mut worst = 0.0_f32;
    for edge in boundaries {
        let low = edge.saturating_sub(window);
        let high = (edge + window).min(frames);
        let was = step_peak(input, channels, low, high);
        let now = step_peak(output, channels, low, high);
        if was > 1e-9 {
            worst = worst.max(now / was);
        }
    }
    worst
}

/// Largest sample step in `[from, to)`, all channels.
fn step_peak(samples: &[f32], channels: usize, from: usize, to: usize) -> f32 {
    let mut worst = 0.0_f32;
    for channel in 0..channels {
        for frame in from.max(1)..to.min(samples.len() / channels) {
            let step = (samples[frame * channels + channel]
                - samples[(frame - 1) * channels + channel])
                .abs();
            worst = worst.max(step);
        }
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;
    use std::time::Instant;

    use rustfft::FftPlanner;
    use rustfft::num_complex::Complex;

    const RATE: u32 = 48_000;

    fn harmonic_tone(f0: f64, seconds: f64, rate: u32) -> Vec<f32> {
        let frames = (f64::from(rate) * seconds) as usize;
        (0..frames)
            .map(|index| {
                let phase = TAU * f0 * index as f64 / f64::from(rate);
                (0.3 * phase.sin() + 0.15 * (2.0 * phase).sin()) as f32
            })
            .collect()
    }

    fn sine(f0: f64, amplitude: f64, seconds: f64, rate: u32) -> Vec<f32> {
        let frames = (f64::from(rate) * seconds) as usize;
        (0..frames)
            .map(|index| {
                let phase = TAU * f0 * index as f64 / f64::from(rate);
                (amplitude * phase.sin()) as f32
            })
            .collect()
    }

    /// Continuous-phase tone whose frequency follows the note list, so the
    /// input itself has no clicks at the note boundaries.
    fn sequence(notes: &[(f64, f64)], amplitude: f64, rate: u32) -> Vec<f32> {
        let total: f64 = notes.iter().map(|(_, seconds)| seconds).sum();
        let frames = (f64::from(rate) * total) as usize;
        let mut samples = Vec::with_capacity(frames);
        let mut phase = 0.0_f64;
        let mut note = 0;
        let mut remaining = (notes[0].1 * f64::from(rate)) as usize;
        for _ in 0..frames {
            while remaining == 0 && note + 1 < notes.len() {
                note += 1;
                remaining = (notes[note].1 * f64::from(rate)) as usize;
            }
            samples.push((amplitude * phase.sin()) as f32);
            phase += TAU * notes[note].0 / f64::from(rate);
            remaining = remaining.saturating_sub(1);
        }
        samples
    }

    fn vibrato_tone(f0: f64, depth_cents: f64, hz: f64, seconds: f64, rate: u32) -> Vec<f32> {
        let frames = (f64::from(rate) * seconds) as usize;
        let mut phase = 0.0_f64;
        let mut samples = Vec::with_capacity(frames);
        for index in 0..frames {
            let time = index as f64 / f64::from(rate);
            let frequency = f0 * 2f64.powf(depth_cents * (TAU * hz * time).sin() / 1_200.0);
            samples.push((0.3 * phase.sin()) as f32);
            phase += TAU * frequency / f64::from(rate);
        }
        samples
    }

    fn white_noise(level: f64, frames: usize) -> Vec<f32> {
        let mut state = 0x5eed_1234_u32;
        (0..frames)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                ((f64::from(state) / f64::from(u32::MAX)) * 2.0 - 1.0) as f32 * level as f32
            })
            .collect()
    }

    fn stereo(left: &[f32], right: &[f32]) -> Vec<f32> {
        let mut buffer = Vec::with_capacity(left.len() * 2);
        for (l, r) in left.iter().zip(right) {
            buffer.push(*l);
            buffer.push(*r);
        }
        buffer
    }

    fn channel(samples: &[f32], channels: usize, wanted: usize) -> Vec<f32> {
        samples
            .iter()
            .skip(wanted)
            .step_by(channels)
            .copied()
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>()
            / samples.len().max(1) as f64)
            .sqrt()
    }

    fn correlation(left: &[f32], right: &[f32]) -> f64 {
        let mut ab = 0.0;
        let mut aa = 0.0;
        let mut bb = 0.0;
        for (l, r) in left.iter().zip(right) {
            ab += f64::from(*l) * f64::from(*r);
            aa += f64::from(*l) * f64::from(*l);
            bb += f64::from(*r) * f64::from(*r);
        }
        ab / (aa * bb).sqrt().max(1e-18)
    }

    /// Normalised autocorrelation peak inside ±5 % of `expected`; an oracle
    /// that shares no code with the detector under test.
    fn measured_f0(samples: &[f32], rate: f64, start: usize, expected: f64, width: usize) -> f64 {
        let nominal = rate / expected;
        let low = (nominal * 0.95).floor().max(2.0) as usize;
        let high = (nominal * 1.05).ceil() as usize;
        let mut curve: Vec<(f64, f64)> = Vec::new();
        for lag in low..=high {
            if start + width + lag >= samples.len() {
                break;
            }
            let mut ab = 0.0;
            let mut aa = 0.0;
            let mut bb = 0.0;
            for offset in 0..width {
                let a = f64::from(samples[start + offset]);
                let b = f64::from(samples[start + offset + lag]);
                ab += a * b;
                aa += a * a;
                bb += b * b;
            }
            curve.push((lag as f64, ab / (aa * bb).sqrt().max(1e-18)));
        }
        let Some(top) = curve
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.1.total_cmp(&right.1.1))
            .map(|(index, _)| index)
        else {
            return expected;
        };
        let mut lag = curve[top].0;
        if top > 0 && top + 1 < curve.len() {
            let offset = psola::parabolic_offset(curve[top - 1].1, curve[top].1, curve[top + 1].1);
            lag += offset;
        }
        rate / lag
    }

    fn cents(measured: f64, target: f64) -> f64 {
        1_200.0 * (measured / target).log2()
    }

    fn median(values: &mut [f64]) -> f64 {
        values.sort_by(|left, right| left.total_cmp(right));
        values[values.len() / 2]
    }

    fn percentile(values: &mut [f64], fraction: f64) -> f64 {
        values.sort_by(|left, right| left.total_cmp(right));
        let index = ((values.len() - 1) as f64 * fraction).round() as usize;
        values[index]
    }

    /// Cents error per 5 ms frame over `[head, tail)` of the buffer.
    fn pitch_track(samples: &[f32], rate: u32, target: f64, head: f64, tail: f64) -> Vec<f64> {
        let rate = f64::from(rate);
        let hop = (rate * 0.005) as usize;
        let width = (rate * 0.025) as usize;
        let guard = (rate / target * 1.1) as usize + 8;
        let from = (head * rate) as usize;
        let to = samples.len() - (tail * rate) as usize;
        let mut track = Vec::new();
        let mut start = from;
        while start + width + guard < to {
            let measured = measured_f0(samples, rate, start, target, width);
            track.push(cents(measured, target));
            start += hop;
        }
        track
    }

    fn blackman_harris(index: usize, len: usize) -> f64 {
        let phase = TAU * index as f64 / len as f64;
        0.358_75 - 0.488_29 * phase.cos() + 0.141_28 * (2.0 * phase).cos()
            - 0.011_68 * (3.0 * phase).cos()
    }

    fn spectrum(samples: &[f32], rate: u32) -> (Vec<f64>, f64) {
        let nfft = samples.len().next_power_of_two();
        let mut planner = FftPlanner::<f64>::new();
        let forward = planner.plan_fft_forward(nfft);
        let mut buffer: Vec<Complex<f64>> = (0..nfft)
            .map(|index| {
                let value = if index < samples.len() {
                    f64::from(samples[index]) * blackman_harris(index, samples.len())
                } else {
                    0.0
                };
                Complex::new(value, 0.0)
            })
            .collect();
        forward.process(&mut buffer);
        let magnitudes = buffer
            .iter()
            .take(nfft / 2)
            .map(|value| value.norm())
            .collect();
        (magnitudes, f64::from(rate) / nfft as f64)
    }

    fn peak_near(magnitudes: &[f64], bin: f64) -> f64 {
        let centre = bin.round() as usize;
        let low = centre.saturating_sub(3);
        let high = (centre + 3).min(magnitudes.len() - 1);
        magnitudes[low..=high]
            .iter()
            .fold(0.0_f64, |worst, value| worst.max(*value))
    }

    /// Loudest spectral peak that is not a harmonic of `f0`, in dBc.
    fn worst_spur_db(magnitudes: &[f64], bin_hz: f64, f0: f64) -> f64 {
        let fundamental = peak_near(magnitudes, f0 / bin_hz);
        let mut worst = f64::NEG_INFINITY;
        for bin in 2..magnitudes.len() - 2 {
            let hz = bin as f64 * bin_hz;
            if hz < f0 * 0.5 {
                continue;
            }
            let nearest = (hz / f0).round().max(1.0) * f0;
            if (hz - nearest).abs() <= 4.0 * bin_hz {
                continue;
            }
            if magnitudes[bin] > magnitudes[bin - 1] && magnitudes[bin] >= magnitudes[bin + 1] {
                worst = worst.max(20.0 * (magnitudes[bin] / fundamental).log10());
            }
        }
        worst
    }

    fn thd(magnitudes: &[f64], bin_hz: f64, f0: f64) -> f64 {
        let fundamental = peak_near(magnitudes, f0 / bin_hz);
        let mut sum = 0.0;
        for harmonic in 2..=20 {
            let bin = harmonic as f64 * f0 / bin_hz;
            if bin + 3.0 >= (magnitudes.len() - 1) as f64 {
                break;
            }
            sum += peak_near(magnitudes, bin).powi(2);
        }
        sum.sqrt() / fundamental
    }

    fn cents_error_of(samples: &[f32], rate: u32, target: f64, start: f64) -> f64 {
        let width = (f64::from(rate) * 0.030) as usize;
        let at = (start * f64::from(rate)) as usize;
        cents(
            measured_f0(samples, f64::from(rate), at, target, width),
            target,
        )
    }

    fn segment(start: usize, end: usize, cents: f32) -> ShiftSegment {
        ShiftSegment::new(start, end, cents)
    }

    /// Deterministic uniform noise in `-1..=1` from an xorshift state.
    fn lcg_unit(state: &mut u32) -> f64 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        (f64::from(*state) / f64::from(u32::MAX)) * 2.0 - 1.0
    }

    /// One two-pole resonator: a vocal-tract formant at `hz` with `bw`.
    fn resonators(samples: &[f64], formants: &[(f64, f64)], rate: f64) -> Vec<f64> {
        let mut out = samples.to_vec();
        for (hz, bw) in formants {
            let radius = (-std::f64::consts::PI * bw / rate).exp();
            let theta = std::f64::consts::TAU * hz / rate;
            let (b0, feedback, feedforward) =
                (1.0 - radius, 2.0 * radius * theta.cos(), -radius * radius);
            let mut previous = 0.0;
            let mut before = 0.0;
            for sample in &mut out {
                let value = b0 * *sample + feedback * previous + feedforward * before;
                before = previous;
                previous = value;
                *sample = value;
            }
        }
        out
    }

    /// Source-filter vowel: glottal pulses with jitter/shimmer, three formants
    /// and a breath floor. Deterministic for a given seed.
    fn pulse_vowel(seconds: f64, rate: u32, f0: f64, breath: f64) -> Vec<f32> {
        pulse_vowel_with_formants(
            seconds,
            rate,
            f0,
            breath,
            &[(700.0, 80.0), (1_220.0, 90.0), (2_600.0, 120.0)],
        )
    }

    fn pulse_vowel_with_formants(
        seconds: f64,
        rate: u32,
        f0: f64,
        breath: f64,
        formants: &[(f64, f64)],
    ) -> Vec<f32> {
        let frames = (seconds * f64::from(rate)) as usize;
        let mut noise = vec![0.0_f64; frames];
        let mut state = 0x2545_f491_u32;
        for sample in &mut noise {
            *sample = lcg_unit(&mut state);
        }
        let mut pulses = vec![0.0_f64; frames];
        let mut position = 0.0_f64;
        while position < frames as f64 {
            let period = (1.0 / f0) * (1.0 + 0.005 * lcg_unit(&mut state));
            let index = position.round() as usize;
            if index < frames {
                pulses[index] += 1.0 + 0.02 * lcg_unit(&mut state);
            }
            position += period * f64::from(rate);
        }
        let mut out = resonators(&pulses, formants, f64::from(rate));
        let rms = (out.iter().map(|value| value * value).sum::<f64>() / frames as f64).sqrt();
        let scale = 0.1 / rms.max(1e-12);
        for (sample, noise) in out.iter_mut().zip(&noise) {
            *sample = *sample * scale + breath * noise;
        }
        out.into_iter().map(|value| value as f32).collect()
    }

    /// Formants read from an independent long-window envelope: the first two
    /// local maxima of a 170 ms cepstral envelope above 200 Hz. The LPC root
    /// finder is biased by the 140 Hz comb and cannot resolve a formant that
    /// landed between two harmonics, so the envelope is the oracle here.
    fn envelope_formants(samples: &[f32], rate: u32) -> Vec<f64> {
        let (envelope, bin_hz) = long_envelope(samples, rate);
        let count = envelope.len();
        let mut peaks: Vec<f64> = Vec::new();
        for bin in 1..count - 1 {
            let hz = bin as f64 * bin_hz;
            if !(200.0..3_500.0).contains(&hz) {
                continue;
            }
            if envelope[bin] > envelope[bin - 1] && envelope[bin] >= envelope[bin + 1] {
                if let Some(last) = peaks.last()
                    && hz - last < 120.0
                {
                    continue;
                }
                peaks.push(hz);
            }
        }
        peaks.truncate(2);
        peaks
    }

    /// Exactly periodic harmonic vowel: the formant resonators shape the
    /// amplitude of each harmonic of `f0`, with no excitation noise at all.
    fn harmonic_vowel(f0: f64, seconds: f64, rate: u32) -> Vec<f32> {
        let rate_f = f64::from(rate);
        let frames = (seconds * rate_f) as usize;
        let formants = [(700.0_f64, 80.0_f64), (1_220.0, 90.0), (2_600.0, 120.0)];
        let envelope = |hz: f64| {
            formants.iter().fold(1.0_f64, |gain, (centre, bandwidth)| {
                let radius = (-std::f64::consts::PI * bandwidth / rate_f).exp();
                let theta = std::f64::consts::TAU * centre / rate_f;
                let omega = std::f64::consts::TAU * hz / rate_f;
                let denominator = Complex::new(1.0, 0.0)
                    - Complex::from_polar(2.0 * radius * theta.cos(), -omega)
                    + Complex::from_polar(radius * radius, -2.0 * omega);
                gain * (1.0 - radius) / denominator.norm()
            })
        };
        let mut out = vec![0.0_f64; frames];
        let mut harmonic = 1;
        loop {
            let hz = f0 * harmonic as f64;
            if hz >= rate_f * 0.5 {
                break;
            }
            let amplitude = envelope(hz);
            let omega = std::f64::consts::TAU * hz / rate_f;
            for (index, sample) in out.iter_mut().enumerate() {
                *sample += amplitude * (omega * index as f64).sin();
            }
            harmonic += 1;
        }
        let rms = (out.iter().map(|value| value * value).sum::<f64>() / frames as f64).sqrt();
        let scale = 0.1 / rms.max(1e-12);
        out.into_iter()
            .map(|value| (value * scale) as f32)
            .collect()
    }

    /// Log-magnitude envelope on a fixed grid, from one long cepstral frame.
    fn long_envelope(samples: &[f32], rate: u32) -> (Vec<f64>, f64) {
        let n = 8_192;
        let start = samples.len() / 3;
        assert!(start + n <= samples.len());
        let mut spectrum: Vec<Complex<f64>> = (0..n)
            .map(|index| {
                let phase = std::f64::consts::TAU * index as f64 / n as f64;
                let window = 0.5 - 0.5 * phase.cos();
                Complex::new(f64::from(samples[start + index]) * window, 0.0)
            })
            .collect();
        let mut planner = FftPlanner::<f64>::new();
        planner.plan_fft_forward(n).process(&mut spectrum);
        let bins = n / 2 + 1;
        let mut log_full = vec![0.0_f64; n];
        let peak = spectrum
            .iter()
            .take(bins)
            .fold(0.0_f64, |worst, value| worst.max(value.norm()));
        let floor = peak * 1e-5;
        for bin in 0..bins {
            log_full[bin] = spectrum[bin].norm().max(floor).ln();
        }
        for bin in 1..n / 2 {
            log_full[n - bin] = log_full[bin];
        }
        let mut cepstrum: Vec<Complex<f64>> = log_full
            .iter()
            .map(|value| Complex::new(*value, 0.0))
            .collect();
        planner.plan_fft_inverse(n).process(&mut cepstrum);
        let scale = 1.0 / n as f64;
        for value in &mut cepstrum {
            *value *= scale;
        }
        for value in cepstrum.iter_mut().take(n - 192).skip(193) {
            *value = Complex::new(0.0, 0.0);
        }
        planner.plan_fft_forward(n).process(&mut cepstrum);
        (
            cepstrum.iter().take(bins).map(|value| value.re).collect(),
            f64::from(rate) / n as f64,
        )
    }

    /// RMS difference of two log-envelope vectors over `[from, to)`, in dB.
    fn lsd(measured: &[f64], expected: &[f64], from: usize, to: usize) -> f64 {
        let mut sum = 0.0;
        let mut count = 0;
        for bin in from..to {
            let difference = 20.0 / 10f64.ln() * (measured[bin] - expected[bin]);
            sum += difference * difference;
            count += 1;
        }
        (sum / count.max(1) as f64).sqrt()
    }

    fn lcg_noise(frames: usize, amplitude: f32) -> Vec<f32> {
        let mut state = 0x1234_5678_u32;
        (0..frames)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 8) as f32 / 16_777_216.0 * 2.0 - 1.0) * amplitude
            })
            .collect()
    }

    fn pulse_train(period: usize, frames: usize, amplitude: f32) -> Vec<f32> {
        (0..frames)
            .map(|index| {
                if index % period < 2 {
                    amplitude
                } else if index % period == period / 2 {
                    -amplitude * 0.5
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn buffer_hash(samples: &[f32]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for sample in samples {
            for byte in sample.to_bits().to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    #[test]
    fn clip_formant_processing_stops_between_regions() {
        let mut samples = vec![0.1; 48_000 * 2];
        let mut regions = 0;
        assert!(!shift_stereo_formants_while(
            &mut samples,
            48_000,
            200.0,
            || {
                regions += 1;
                regions < 3
            }
        ));
        assert_eq!(regions, 3);
        assert!(samples.iter().all(|sample| *sample == 0.1));
    }

    #[test]
    fn formant_shift_lands_within_five_percent_and_keeps_pitch() {
        let input = pulse_vowel(2.0, RATE, 140.0, 0.0015);
        let source = envelope_formants(&input, RATE);
        assert_eq!(source.len(), 2, "oracle found {source:?}");
        for formant_cents in [-200.0_f32, -150.0, 150.0, 200.0] {
            let curve = ShiftCurve::constant_with_formant(input.len(), 0.0, formant_cents);
            let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
            assert!(output.samples.iter().all(|sample| sample.is_finite()));
            let ratio = 2f64.powf(f64::from(formant_cents) / 1_200.0);
            let measured = envelope_formants(&output.samples, RATE);
            assert_eq!(measured.len(), 2, "{formant_cents}: {measured:?}");
            for (before, after) in source.iter().zip(&measured) {
                let expected = before * ratio;
                let error = (after - expected) / expected;
                println!(
                    "formant {formant_cents:+.0} ct: {before:.1} -> {after:.1} Hz (want {expected:.1}, {:+.2} %)",
                    error * 100.0
                );
                assert!(
                    error.abs() <= 0.05,
                    "F {before:.1} Hz moved to {after:.1} Hz, want {expected:.1}"
                );
            }
            // A formant-only move leaves the pitch on its target.
            let pitch = measured_f0(
                &output.samples,
                f64::from(RATE),
                RATE as usize / 2,
                140.0,
                (RATE as f64 * 0.03) as usize,
            );
            let error = cents(pitch, 140.0);
            assert!(error.abs() <= 2.0, "pitch moved {error:+.2} cents");
        }
        // Pitch and formant move together: the pitch still lands within 2
        // cents. The LPC oracle is not used for formants here because the
        // pitch-shifted comb biases it; the formant-only cases above carry that
        // gate, and `notes/vocal5.md` records the combined envelope reading.
        for (formant_cents, shift_cents) in [(-200.0_f32, 100.0_f32), (200.0, -100.0)] {
            let curve = ShiftCurve::constant_with_formant(input.len(), shift_cents, formant_cents);
            let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
            let target = 140.0 * 2f64.powf(f64::from(shift_cents) / 1_200.0);
            let pitch = measured_f0(
                &output.samples,
                f64::from(RATE),
                RATE as usize / 2,
                target,
                (RATE as f64 * 0.03) as usize,
            );
            let error = cents(pitch, target);
            println!("formant {formant_cents:+.0}, pitch {shift_cents:+.0}: {error:+.2} cents");
            assert!(
                error.abs() <= 2.0,
                "formant {formant_cents:+.0} moved the pitch {error:+.2} cents"
            );
        }
    }

    #[test]
    fn wide_clip_formants_keep_unvoiced_attacks_and_reshape_voiced_vowels() {
        let vowel = pulse_vowel(0.55, RATE, 165.0, 0.0015);
        let noise = white_noise(0.035, RATE as usize / 8);
        let mut mono = noise.clone();
        mono.extend_from_slice(&vowel);
        let tail = mono.len();
        mono.extend_from_slice(&noise);
        let dry = stereo(&mono, &mono);
        for cents in [-900.0, 900.0] {
            let mut shifted = dry.clone();
            shift_stereo_formants(&mut shifted, RATE, cents);
            let protected = RATE as usize / 24;
            assert_eq!(shifted[..protected * 2], dry[..protected * 2]);
            assert_eq!(
                shifted[(tail + protected) * 2..],
                dry[(tail + protected) * 2..]
            );
            let middle = (noise.len() + vowel.len() / 3) * 2;
            let end = middle + (RATE as usize / 8) * 2;
            assert!(
                shifted[middle..end]
                    .iter()
                    .zip(&dry[middle..end])
                    .any(|(&wet, &dry)| (wet - dry).abs() > 0.002)
            );
            assert!(shifted.iter().all(|sample| sample.is_finite()));
        }
    }

    #[test]
    fn wide_clip_formants_preserve_recorded_high_frequency_rolloff() {
        let rate = 44_100;
        let period_frames = 300;
        let f0 = f64::from(rate) / period_frames as f64;
        let period: Vec<f32> = (0..period_frames)
            .map(|frame| {
                (1..period_frames / 2)
                    .map(|harmonic| {
                        let frequency = harmonic as f64 * f0;
                        let rolloff = (1.0 + (frequency / 12_000.0).powi(16)).sqrt();
                        let envelope = [(700.0, 100.0), (1_220.0, 140.0), (2_600.0, 240.0)]
                            .into_iter()
                            .map(|(centre, width)| {
                                1.0 + 6.0 / (1.0 + ((frequency - centre) / width).powi(2))
                            })
                            .product::<f64>();
                        let phase = 2.0 * PI * (harmonic * frame) as f64 / period_frames as f64;
                        envelope * phase.cos() / (harmonic as f64 * rolloff)
                    })
                    .sum::<f64>() as f32
            })
            .collect();
        let peak = period.iter().map(|sample| sample.abs()).fold(0.0, f32::max);
        let mono: Vec<_> = period.iter().map(|sample| sample * 0.25 / peak).collect();
        let mono = mono.repeat(90);
        let dry = stereo(&mono, &mono);
        let fft = FftPlanner::<f64>::new().plan_fft_forward(period_frames);
        let power = |samples: &[f32]| {
            let start = 45 * period_frames;
            let mut bins: Vec<_> = samples[start * 2..(start + period_frames) * 2]
                .chunks_exact(2)
                .map(|frame| Complex::new(f64::from(frame[0]), 0.0))
                .collect();
            fft.process(&mut bins);
            bins.iter().map(Complex::norm_sqr).collect::<Vec<_>>()
        };
        let air_power = |bins: &[f64]| {
            let start = (10_000.0 / f0).ceil() as usize;
            let end = (18_000.0 / f0).floor() as usize;
            bins[start..=end].iter().sum::<f64>()
        };
        let dry_power = power(&dry);
        for cents in [-900.0, 900.0] {
            let mut shifted = dry.clone();
            shift_stereo_formants(&mut shifted, rate, cents);
            let wet_power = power(&shifted);
            let air_db = 10.0 * (air_power(&wet_power) / air_power(&dry_power)).log10();
            assert!(
                air_db.abs() < 1.0,
                "{cents:+} cents moved air {air_db:+.3} dB"
            );
            assert!(
                (3..30)
                    .any(|bin| { (10.0 * (wet_power[bin] / dry_power[bin]).log10()).abs() > 1.0 })
            );
        }
    }

    #[test]
    fn high_voice_envelope_rejects_the_excitation_comb() {
        let period = 128;
        let frames = period * 160;
        let impulse_train: Vec<_> = (0..frames)
            .map(|frame| if frame % period == 0 { 0.7 } else { 0.0 })
            .collect();
        let config = ShiftConfig::new(RATE);
        let mut estimator = EnvelopeEstimator::new(
            config.formant_window_frames,
            config.formant_hop_frames,
            config.formant_lifter_frames,
            false,
        );
        let track = Track {
            sample_rate: RATE,
            frames: vec![(0.0, period as f64)],
            voiced: vec![(0, frames)],
        };
        let fixed = estimator
            .estimate(&impulse_train, 1, frames, 2_048, frames - 2_048, None)
            .unwrap();
        let adaptive = estimator
            .estimate(
                &impulse_train,
                1,
                frames,
                2_048,
                frames - 2_048,
                Some(&track),
            )
            .unwrap();
        let variation = |curve: &[f64]| {
            let interior = &curve[20..200];
            let min = interior.iter().copied().fold(f64::INFINITY, f64::min);
            let max = interior.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            max - min
        };
        assert!(variation(&fixed) > 0.1);
        assert!(variation(&adaptive) < variation(&fixed) * 0.25);
    }

    #[test]
    fn wide_formant_filter_excludes_consonants_from_voiced_context() {
        let config = ShiftConfig::new(RATE);
        let prefix = RATE as usize / 8;
        let vowel = pulse_vowel(0.55, RATE, 165.0, 0.0015);
        let end = prefix + vowel.len();
        let frames = end + prefix;
        let consonants = white_noise(0.25, prefix);
        let make_take = |polarity: f32| {
            let mut mono: Vec<_> = consonants.iter().map(|sample| polarity * sample).collect();
            mono.extend_from_slice(&vowel);
            mono.extend(consonants.iter().map(|sample| polarity * sample));
            stereo(&mono, &mono)
        };
        for cents in [-900.0, 900.0] {
            let mut a = make_take(1.0);
            let mut b = make_take(0.0);
            let segment = ShiftSegment {
                start: 0,
                end: frames,
                cents: 0.0,
                formant_cents: cents,
            };
            let voice_track = Track {
                sample_rate: RATE,
                frames: Vec::new(),
                voiced: vec![(prefix, end)],
            };
            for take in [&mut a, &mut b] {
                reshape_envelope(
                    take,
                    2,
                    &[segment],
                    &config,
                    config.ramp_frames,
                    Some(&voice_track),
                    || true,
                )
                .unwrap();
            }
            let middle = prefix + vowel.len() / 2;
            assert!(
                (middle..middle + 128)
                    .any(|frame| { (a[frame * 2] - vowel[frame - prefix]).abs() > 0.002 })
            );
            let protected = config.formant_window_frames / 2;
            assert_eq!(
                channel(&a, 2, 0)[prefix..prefix + protected],
                vowel[..protected]
            );
            assert!(
                a[prefix * 2..end * 2]
                    .iter()
                    .zip(&b[prefix * 2..end * 2])
                    .all(|(a, b)| (a - b).abs() < 1e-6)
            );
        }
    }

    #[test]
    fn clip_formant_shift_handles_a_voice_that_cancels_in_mono() {
        let mono = pulse_vowel_with_formants(
            0.8,
            RATE,
            145.0,
            0.0015,
            &[(550.0, 80.0), (1_300.0, 100.0), (2_500.0, 150.0)],
        );
        let mut center: Vec<_> = mono.iter().flat_map(|&sample| [sample, sample]).collect();
        let mut side: Vec<_> = mono.iter().flat_map(|&sample| [sample, -sample]).collect();
        shift_stereo_formants(&mut center, RATE, 600.0);
        shift_stereo_formants(&mut side, RATE, 600.0);
        assert!(
            side.chunks_exact(2)
                .zip(&mono)
                .any(|(frame, dry)| (frame[0] - dry).abs() > 1e-4)
        );
        for (actual, expected) in side.chunks_exact(2).zip(center.chunks_exact(2)) {
            assert!((actual[0] - expected[0]).abs() < 1e-6);
            assert!((actual[0] + actual[1]).abs() < 1e-6);
        }
    }

    #[test]
    fn clip_formant_preservation_tracks_distinct_vowels_and_keeps_sibilance() {
        let profiles = [
            [(700.0, 80.0), (1_220.0, 90.0), (2_600.0, 120.0)],
            [(410.0, 75.0), (1_800.0, 110.0), (2_950.0, 160.0)],
            [(560.0, 85.0), (970.0, 90.0), (2_400.0, 140.0)],
        ];
        let pitch_ratio = 2.0_f64.powf(3.0 / 12.0);
        let target_f0 = 140.0 * pitch_ratio;
        let mut shifted = Vec::new();
        let mut ranges = Vec::new();
        let mut originals = Vec::new();
        for formants in profiles {
            let original = pulse_vowel_with_formants(0.8, RATE, 140.0, 0.0015, &formants);
            originals.push(original);
            let moved = formants
                .map(|(frequency, bandwidth)| (frequency * pitch_ratio, bandwidth * pitch_ratio));
            let raised = pulse_vowel_with_formants(0.8, RATE, target_f0, 0.0015, &moved);
            let start = shifted.len();
            shifted.extend_from_slice(&raised);
            ranges.push(start..shifted.len());
            let mut seed = 0xaadb_cdef;
            let mut previous = 0.0;
            for _ in 0..RATE / 6 {
                let noise = lcg_unit(&mut seed);
                shifted.push(((noise - previous) * 0.035) as f32);
                previous = noise;
            }
        }
        let mut stereo: Vec<_> = shifted
            .iter()
            .flat_map(|&sample| [sample, sample])
            .collect();
        shift_stereo_formants(&mut stereo, RATE, -300.0);
        let preserved = channel(&stereo, 2, 0);
        let strongest_near = |samples: &[f32], nominal: f64| {
            let (envelope, bin_hz) = long_envelope(samples, RATE);
            let lo = (nominal * 0.83 / bin_hz).ceil() as usize;
            let hi = (nominal * 1.38 / bin_hz).floor() as usize;
            (lo..=hi)
                .max_by(|&a, &b| envelope[a].total_cmp(&envelope[b]))
                .unwrap() as f64
                * bin_hz
        };
        for (index, range) in ranges.iter().enumerate() {
            let raw = &originals[index];
            for (nominal, _) in profiles[index].into_iter().take(2) {
                let reference = strongest_near(raw, nominal);
                let raised = strongest_near(&shifted[range.clone()], nominal);
                let restored = strongest_near(&preserved[range.clone()], nominal);
                println!(
                    "vowel {index} {nominal:.0} Hz: raw {reference:.1}, shifted {raised:.1}, preserved {restored:.1}"
                );
                assert!((restored - reference).abs() <= 0.05 * reference);
                assert!((restored - reference).abs() < (raised - reference).abs());
            }
            let f0 = measured_f0(
                &preserved[range.clone()],
                f64::from(RATE),
                range.len() / 2,
                target_f0,
                (RATE as f64 * 0.03) as usize,
            );
            assert!(cents(f0, target_f0).abs() <= 5.0);
            let unvoiced = range.end + RATE as usize / 24..range.end + RATE as usize / 8;
            let level_delta =
                20.0 * (rms(&preserved[unvoiced.clone()]) / rms(&shifted[unvoiced])).log10();
            println!("vowel {index} unvoiced level delta: {level_delta:.2} dB");
            assert!(level_delta.abs() < 1.0);
        }
    }

    #[test]
    fn zero_formant_is_bit_identical_to_the_pitch_only_shifter() {
        // Hashes of the 1.3 shifter's output on this fixed buffer, captured
        // before the formant pass existed. They must not move.
        let frames = 24_000;
        let left = pulse_train(218, frames, 0.4);
        let right: Vec<f32> = left
            .iter()
            .zip(lcg_noise(frames, 0.02))
            .map(|(a, b)| a + b)
            .collect();
        let stereo_input = stereo(&left, &right);
        for (cents, mono_hash, stereo_hash) in [
            (
                -300.0_f32,
                0x55de_4f5b_93ed_c088_u64,
                0xc82b_625b_defe_4711_u64,
            ),
            (0.0, 0x8beb_264a_2cf9_30bd, 0xefb2_4603_79ce_b6a6),
            (100.0, 0xbef4_abad_eced_ac75, 0xf872_722e_b2fa_4b43),
            (700.0, 0x35fc_28c2_c953_b894, 0xd80e_0a1e_8314_a63e),
        ] {
            let plain = ShiftCurve::constant(frames, cents);
            let explicit = ShiftCurve::constant_with_formant(frames, cents, 0.0);
            assert_eq!(plain.segments(), explicit.segments());
            let mono = shift(&left, 1, RATE, &explicit, &ShiftConfig::new(RATE)).unwrap();
            let pair = shift(&stereo_input, 2, RATE, &explicit, &ShiftConfig::new(RATE)).unwrap();
            assert_eq!(buffer_hash(&mono.samples), mono_hash, "{cents} ct mono");
            assert_eq!(buffer_hash(&pair.samples), stereo_hash, "{cents} ct stereo");
            let reference = shift(&left, 1, RATE, &plain, &ShiftConfig::new(RATE)).unwrap();
            assert_eq!(reference.samples, mono.samples);
        }
    }

    #[test]
    fn formant_sidebands_stay_below_the_budget_and_keep_the_pitch() {
        let input = harmonic_vowel(200.0, 1.5, RATE);
        for formant_cents in [-300.0_f32, 300.0] {
            let curve = ShiftCurve::constant_with_formant(input.len(), 0.0, formant_cents);
            let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
            let (magnitudes, bin_hz) = steady_spectrum(&output.samples, RATE);
            let spur = worst_spur_db(&magnitudes, bin_hz, 200.0);
            println!("formant {formant_cents:+.0}: worst non-harmonic peak {spur:.1} dBc");
            assert!(spur <= -55.0, "worst spur {spur:.1} dBc");
            let measured = measured_f0(
                &output.samples,
                f64::from(RATE),
                RATE as usize / 2,
                200.0,
                (RATE as f64 * 0.03) as usize,
            );
            let error = cents(measured, 200.0);
            assert!(
                error.abs() <= 2.0,
                "a {formant_cents:+.0} ct formant shift moved the pitch {error:+.2} cents"
            );
        }
    }

    #[test]
    fn formant_log_spectral_distance_tracks_the_request() {
        let input = pulse_vowel(2.0, RATE, 140.0, 0.0015);
        let (source, bin_hz) = long_envelope(&input, RATE);
        let low = (100.0 / bin_hz) as usize;
        let high = (5_000.0 / bin_hz) as usize;
        for cents in [
            -1_200.0_f32,
            -700.0,
            -300.0,
            -200.0,
            -100.0,
            100.0,
            200.0,
            300.0,
            700.0,
            1_200.0,
        ] {
            let curve = ShiftCurve::constant_with_formant(input.len(), 0.0, cents);
            let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
            let (shifted, _) = long_envelope(&output.samples, RATE);
            let ratio = 2f64.powf(f64::from(cents) / 1_200.0);
            let last = source.len() - 1;
            let request: Vec<f64> = (0..source.len())
                .map(|bin| {
                    let read = bin as f64 / ratio;
                    if read >= last as f64 {
                        source[last]
                    } else {
                        let index = read.floor() as usize;
                        let fraction = read - index as f64;
                        source[index] + (source[index + 1] - source[index]) * fraction
                    }
                })
                .collect();
            let measured = lsd(&shifted, &source, low, high);
            let expected = lsd(&request, &source, low, high);
            println!("formant {cents:+.0}: LSD {measured:.2} dB, request {expected:.2} dB");
            assert!(
                (measured - expected).abs() <= 0.5 + 0.05 * expected,
                "{cents:+.0} ct: measured {measured:.2} dB vs request {expected:.2} dB"
            );
        }
    }

    #[test]
    fn formant_shifts_are_deterministic_and_finite_at_the_limits() {
        let input = pulse_vowel(1.2, RATE, 140.0, 0.0015);
        let head = RATE as usize / 4;
        for cents in [-1_200.0_f32, -700.0, -300.0, 300.0, 700.0, 1_200.0] {
            let curve = ShiftCurve::constant_with_formant(input.len(), 0.0, cents);
            let first = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
            let second = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
            assert_eq!(
                first.samples, second.samples,
                "{cents} is not deterministic"
            );
            assert!(
                first.samples.iter().all(|sample| sample.is_finite()),
                "{cents} produced a non-finite sample"
            );
            let relation = correlation(&input[head..], &first.samples[head..]);
            assert!(
                relation > 0.0,
                "{cents} inverted the waveform (correlation {relation:.3})"
            );
        }
    }

    #[test]
    fn a_hundred_cent_shift_lands_within_two_cents() {
        let input = harmonic_tone(220.0, 3.0, RATE);
        let target = 220.0 * 2f64.powf(100.0 / 1_200.0);
        let curve = ShiftCurve::constant(input.len(), 100.0);
        let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        assert_eq!(output.samples.len(), input.len());

        let track = pitch_track(&output.samples, RATE, target, 0.2, 0.2);
        assert!(track.len() > 400, "{} frames", track.len());
        let median_error = median(&mut track.clone());
        let p95 = percentile(&mut track.clone(), 0.95);
        let mean = track.iter().sum::<f64>() / track.len() as f64;
        let jitter = (track
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / track.len() as f64)
            .sqrt();
        println!(
            "100 cents: median {median_error:+.3}, p95 {:.3}, jitter {jitter:.3} cents over {} frames",
            p95.abs(),
            track.len()
        );
        assert!(p95.abs() <= 2.0, "p95 {p95:.3} cents");
        assert!(jitter <= 2.0, "jitter {jitter:.3} cents");
    }

    #[test]
    fn eight_detuned_notes_land_within_five_cents() {
        let note = 0.5_f64;
        let semitones = [60_u32, 62, 64, 65, 67, 69, 71, 72];
        let notes: Vec<(f64, f64)> = semitones
            .iter()
            .map(|midi| (440.0 * 2f64.powf((f64::from(*midi) - 69.0) / 12.0), note))
            .collect();
        let input = sequence(&notes, 0.3, RATE);
        let per_note = (f64::from(RATE) * note) as usize;
        let segments: Vec<ShiftSegment> = (0..notes.len())
            .map(|index| segment(index * per_note, (index + 1) * per_note, -40.0))
            .collect();
        let curve = ShiftCurve::from_segments(input.len(), &segments).unwrap();
        let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        for (index, (f0, _)) in notes.iter().enumerate() {
            let target = f0 * 2f64.powf(-40.0 / 1_200.0);
            let error = cents_error_of(&output.samples, RATE, target, index as f64 * note + 0.1);
            println!("note {index}: {:+.3} cents", error);
            assert!(error.abs() <= 5.0, "note {index} off by {error:.3} cents");
        }
    }

    #[test]
    fn vibrato_survives_the_shift() {
        let input = vibrato_tone(220.0, 25.0, 6.0, 3.0, RATE);
        let nominal = 220.0 * 2f64.powf(100.0 / 1_200.0);
        let curve = ShiftCurve::constant(input.len(), 100.0);
        let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        let track = pitch_track(&output.samples, RATE, nominal, 0.2, 0.2);
        let depth =
            (percentile(&mut track.clone(), 0.95) - percentile(&mut track.clone(), 0.05)) / 2.0;
        let jump = track
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f64, f64::max);
        println!("vibrato: depth {depth:.2} cents, worst frame jump {jump:.2} cents");
        assert!(
            (depth - 25.0).abs() <= 2.5,
            "depth {depth:.2} cents outside ±10 %"
        );
        assert!(
            jump <= 15.0,
            "frame jump {jump:.2} cents looks like a re-trigger"
        );

        let mut flatten = ShiftConfig::new(RATE);
        flatten.preserve_period_contour = false;
        let flat = shift(&input, 1, RATE, &curve, &flatten).unwrap();
        let flat_track = pitch_track(&flat.samples, RATE, nominal, 0.2, 0.2);
        let flat_depth = (percentile(&mut flat_track.clone(), 0.95)
            - percentile(&mut flat_track.clone(), 0.05))
            / 2.0;
        println!("flattened contour: depth {flat_depth:.2} cents");
        assert!(flat_depth < depth * 0.5);
    }

    #[test]
    fn grain_sidebands_stay_below_the_budget() {
        let input = harmonic_tone(220.0, 1.5, RATE);
        let curve = ShiftCurve::constant(input.len(), 100.0);
        let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        let head = (f64::from(RATE) * 0.15) as usize;
        let tail = (f64::from(RATE) * 0.15) as usize;
        let steady = &output.samples[head..output.samples.len() - tail];
        let (magnitudes, bin_hz) = spectrum(steady, RATE);
        let f0 = 220.0 * 2f64.powf(100.0 / 1_200.0);
        let spur = worst_spur_db(&magnitudes, bin_hz, f0);
        println!("worst non-harmonic peak {spur:.1} dBc");
        assert!(spur <= -55.0, "worst spur {spur:.1} dBc");
    }

    #[test]
    fn thd_of_a_shifted_kilohertz_tone_stays_under_two_percent() {
        let input = sine(1_000.0, 0.3, 1.0, RATE);
        let curve = ShiftCurve::constant(input.len(), 100.0);
        let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        let head = (f64::from(RATE) * 0.15) as usize;
        let tail = (f64::from(RATE) * 0.15) as usize;
        let steady = &output.samples[head..output.samples.len() - tail];
        let (magnitudes, bin_hz) = spectrum(steady, RATE);
        let f0 = 1_000.0 * 2f64.powf(100.0 / 1_200.0);
        let distortion = thd(&magnitudes, bin_hz, f0);
        println!("THD at +100 cents, 1 kHz: {:.4} %", distortion * 100.0);
        assert!(distortion <= 0.02, "THD {:.3} %", distortion * 100.0);
    }

    #[test]
    fn the_stereo_image_survives_the_shift() {
        let left = harmonic_tone(220.0, 2.0, RATE);
        let noise = white_noise(0.01, left.len());
        let right: Vec<f32> = left.iter().zip(&noise).map(|(l, n)| 0.9 * l + n).collect();
        let input = stereo(&left, &right);
        let curve = ShiftCurve::constant(left.len(), -200.0);
        let output = shift(&input, 2, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        let head = (f64::from(RATE) * 0.2) as usize;
        let tail = (f64::from(RATE) * 0.2) as usize;
        let before = correlation(
            &left[head..left.len() - tail],
            &right[head..right.len() - tail],
        );
        let out_left = channel(&output.samples, 2, 0);
        let out_right = channel(&output.samples, 2, 1);
        let after = correlation(
            &out_left[head..out_left.len() - tail],
            &out_right[head..out_right.len() - tail],
        );
        println!("stereo correlation {before:.5} -> {after:.5}");
        assert!(before > 0.95);
        assert!(after > 0.95, "correlation collapsed to {after:.5}");
        assert!(
            (after - before).abs() <= 0.01,
            "correlation moved from {before:.5} to {after:.5}"
        );

        let hard_right = vec![0.0_f32; left.len()];
        let panned = stereo(&left, &hard_right);
        let shifted = shift(&panned, 2, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        let right_after = channel(&shifted.samples, 2, 1);
        assert!(
            right_after.iter().all(|sample| *sample == 0.0),
            "a hard-panned source leaked into the empty channel"
        );
        let left_after = channel(&shifted.samples, 2, 0);
        assert!(rms(&left_after) > 0.1);
    }

    #[test]
    fn zero_cents_is_bit_identical() {
        let tone = harmonic_tone(220.0, 1.0, RATE);
        let input = stereo(&tone, &tone);
        for curve in [
            ShiftCurve::none(tone.len()),
            ShiftCurve::constant(tone.len(), 0.0),
            ShiftCurve::from_segments(tone.len(), &[segment(0, tone.len(), 0.0)]).unwrap(),
        ] {
            let output = shift(&input, 2, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
            assert_eq!(output.samples, input, "a zero curve changed the buffer");
            assert_eq!(output.report.shifted_frames, 0);
            assert_eq!(output.report.wet_frames, 0);
        }
        let shifted = shift(
            &input,
            2,
            RATE,
            &ShiftCurve::constant(tone.len(), 300.0),
            &ShiftConfig::new(RATE),
        )
        .unwrap();
        assert_ne!(shifted.samples, input, "a real shift changed nothing");
    }

    #[test]
    fn a_no_op_segment_inside_a_shifted_buffer_is_bit_identical() {
        let input = harmonic_tone(220.0, 2.0, RATE);
        let frames = input.len();
        let gate = (f64::from(RATE) * 0.5) as usize;
        let quiet = (f64::from(RATE) * 1.2) as usize;
        let curve = ShiftCurve::from_segments(
            frames,
            &[
                segment(0, gate, 200.0),
                segment(gate, quiet, 0.0),
                segment(quiet, frames, -100.0),
            ],
        )
        .unwrap();
        let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        assert_eq!(
            &output.samples[gate..quiet],
            &input[gate..quiet],
            "the zero-cents span was resynthesised"
        );
        assert_ne!(&output.samples[0..gate], &input[0..gate]);
        assert_ne!(&output.samples[quiet..], &input[quiet..]);
    }

    #[test]
    fn a_segment_boundary_in_a_sustained_note_does_not_click() {
        let input = harmonic_tone(220.0, 2.0, RATE);
        let boundary = RATE as usize;
        let curve = ShiftCurve::from_segments(
            input.len(),
            &[
                segment(0, boundary, -200.0),
                segment(boundary, input.len(), 200.0),
            ],
        )
        .unwrap();
        let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        println!(
            "segment boundary: step {:.3}x",
            output.report.boundary_step_ratio
        );
        assert!(
            output.report.boundary_step_ratio <= 1.5,
            "boundary step {:.3}x",
            output.report.boundary_step_ratio
        );
    }

    #[test]
    fn a_hard_wet_dry_switch_is_reported_as_a_transient() {
        let input = harmonic_tone(220.0, 2.0, RATE);
        let start = (f64::from(RATE) * 0.5) as usize;
        let end = (f64::from(RATE) * 1.5) as usize;
        let curve = ShiftCurve::from_segments(input.len(), &[segment(start, end, -400.0)]).unwrap();
        let mut hard = ShiftConfig::new(RATE);
        hard.ramp_frames = 0;
        hard.crossfade_frames = 0;
        let clashed = shift(&input, 1, RATE, &curve, &hard).unwrap();
        let ramped = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        println!(
            "hard switch step {:.1}x, ramped step {:.2}x",
            clashed.report.boundary_step_ratio, ramped.report.boundary_step_ratio
        );
        assert!(
            clashed.report.boundary_step_ratio > 5.0,
            "the transient metric missed a hard switch"
        );
        assert!(
            ramped.report.boundary_step_ratio <= 1.5,
            "the ramped default still clicks ({:.2}x)",
            ramped.report.boundary_step_ratio
        );
    }

    #[test]
    fn a_boundary_at_silence_may_switch_hard() {
        let frames = RATE as usize;
        let mut input = vec![0.0_f32; frames];
        let span = (f64::from(RATE) * 0.4) as usize;
        let tone = harmonic_tone(220.0, 0.4, RATE);
        input[span..span + tone.len()].copy_from_slice(&tone);
        let curve =
            ShiftCurve::from_segments(frames, &[segment(span, span + tone.len(), 300.0)]).unwrap();
        let mut hard = ShiftConfig::new(RATE);
        hard.ramp_frames = 0;
        hard.crossfade_frames = 0;
        let output = shift(&input, 1, RATE, &curve, &hard).unwrap();
        assert!(output.samples.iter().all(|sample| sample.is_finite()));
        println!(
            "hard switch at silence: step {:.2}x",
            output.report.boundary_step_ratio
        );
        assert!(
            output.report.boundary_step_ratio <= 1.5,
            "a switch at silence clicked ({:.2}x)",
            output.report.boundary_step_ratio
        );
    }

    #[test]
    fn awkward_buffers_return_audio_without_nan() {
        let cases: Vec<(&str, Vec<f32>, f32)> = vec![
            ("silence", vec![0.0; RATE as usize], 100.0),
            ("shorter than a grain", vec![0.0; 64], 100.0),
            ("one sample", vec![0.25], -100.0),
            (
                "20 ms blip",
                {
                    let mut buffer = vec![0.0_f32; (f64::from(RATE) * 0.1) as usize];
                    let blip = harmonic_tone(220.0, 0.02, RATE);
                    buffer[..blip.len()].copy_from_slice(&blip);
                    buffer
                },
                300.0,
            ),
            ("unvoiced noise", white_noise(0.2, RATE as usize), -700.0),
            ("extreme lift", harmonic_tone(220.0, 0.5, RATE), 1_200.0),
            ("extreme drop", harmonic_tone(220.0, 0.5, RATE), -1_200.0),
        ];
        for (label, input, cents) in cases {
            let curve = ShiftCurve::constant(input.len(), cents);
            let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE))
                .unwrap_or_else(|error| panic!("{label}: {error}"));
            assert_eq!(output.samples.len(), input.len(), "{label}");
            assert!(
                output.samples.iter().all(|sample| sample.is_finite()),
                "{label} produced a non-finite sample"
            );
            assert!(output.report.peak.is_finite(), "{label}");
        }

        let tone = harmonic_tone(220.0, 0.5, RATE);
        let curve = ShiftCurve::constant(tone.len(), 1_200.0);
        let output = shift(&tone, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        let measured = cents_error_of(&output.samples, RATE, 440.0, 0.15);
        println!("+1200 cents: {measured:+.2} cents from 440 Hz");
        assert!(measured.abs() <= 10.0);

        assert_eq!(
            shift(&tone, 3, RATE, &curve, &ShiftConfig::new(RATE)),
            Err(ShiftError::ChannelCount(3))
        );
        assert_eq!(
            shift(&tone, 1, 0, &curve, &ShiftConfig::new(RATE)),
            Err(ShiftError::ZeroSampleRate)
        );
        let mut ragged = tone.clone();
        ragged.push(0.0);
        assert_eq!(
            shift(&ragged, 2, RATE, &curve, &ShiftConfig::new(RATE)),
            Err(ShiftError::RaggedBuffer)
        );
        let mut poisoned = tone.clone();
        poisoned[10] = f32::NAN;
        assert_eq!(
            shift(&poisoned, 1, RATE, &curve, &ShiftConfig::new(RATE)),
            Err(ShiftError::NonFiniteInput)
        );
        let short = ShiftCurve::constant(tone.len() / 2, 100.0);
        assert_eq!(
            shift(&tone, 1, RATE, &short, &ShiftConfig::new(RATE)),
            Err(ShiftError::CurveLength {
                curve: tone.len() / 2,
                frames: tone.len(),
            })
        );
    }

    #[test]
    fn curve_validation_rejects_bad_segments() {
        assert_eq!(
            ShiftCurve::from_segments(100, &[segment(10, 20, f32::NAN)]),
            Err(ShiftError::NonFiniteCents)
        );
        assert_eq!(
            ShiftCurve::from_segments(100, &[segment(20, 20, 10.0)]),
            Err(ShiftError::EmptySegment)
        );
        assert_eq!(
            ShiftCurve::from_segments(100, &[segment(0, 60, 10.0), segment(50, 90, 10.0)]),
            Err(ShiftError::OverlappingSegments)
        );
        let curve =
            ShiftCurve::from_segments(100, &[segment(80, 400, 5_000.0), segment(0, 10, -5_000.0)])
                .unwrap();
        assert_eq!(curve.segments().len(), 2);
        assert_eq!(curve.segments()[1].end, 100);
        assert_eq!(curve.segments()[0].cents, -MAX_SHIFT_CENTS);
        assert_eq!(curve.cents_at(5), -MAX_SHIFT_CENTS);
        assert_eq!(curve.cents_at(50), 0.0);
    }

    #[test]
    fn a_span_that_starts_in_silence_is_still_shifted() {
        // A detected note span may begin a few ms before the first sample that
        // carries signal; that silent head must not drop the whole run.
        let mut input = vec![0.0_f32; RATE as usize / 5];
        let tone = harmonic_tone(220.0, 1.0, RATE);
        input.extend_from_slice(&tone);
        input.extend(std::iter::repeat_n(0.0, RATE as usize / 5));
        let start = RATE as usize / 5 - (0.017 * f64::from(RATE)) as usize;
        let end = RATE as usize / 5 + tone.len();
        let curve = ShiftCurve::from_segments(input.len(), &[segment(start, end, 100.0)]).unwrap();
        let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
        assert!(output.report.wet_frames > 0, "run dropped");
        let target = 220.0 * 2f64.powf(100.0 / 1_200.0);
        let error = cents_error_of(&output.samples, RATE, target, 0.6);
        println!("silent-head span: {error:+.3} cents");
        assert!(error.abs() <= 2.0, "{error:+.3} cents");
        // The silence itself stays bit-identical.
        assert_eq!(input[..start], output.samples[..start]);
    }

    #[test]
    fn a_segment_can_carry_its_own_slew() {
        let default = 50;
        let lone = [segment(100, 1_100, 100.0), segment(1_100, 2_100, 300.0)];
        let shared = ShiftCurve::from_segments(2_200, &lone)
            .unwrap()
            .ramped(default);
        // Adjacent segments: the second one slews in from the first over the
        // call-wide width, so it is still near 100 cents a few frames in.
        assert!(shared[1_100] < 110.0, "{}", shared[1_100]);
        assert!(shared[1_107] < 120.0, "{}", shared[1_107]);

        let own = [
            (segment(100, 1_100, 100.0), None),
            (segment(1_100, 2_100, 300.0), Some(8)),
        ];
        let curve = ShiftCurve::from_ramped_segments(2_200, &own).unwrap();
        let cents = curve.ramped(default);
        // The override only changes the segment that carries it.
        assert!(cents[100] < 1.0, "{}", cents[100]);
        assert!(cents[1_100] < 120.0, "{}", cents[1_100]);
        assert!(cents[1_107] > 290.0, "{}", cents[1_107]);
        // A slew wider than the segment is clipped to it, and zero is a hard
        // switch, exactly like the call-wide ramp.
        let clipped = ShiftCurve::from_ramped_segments(
            400,
            &[
                (segment(0, 4, 100.0), Some(9_000)),
                (segment(200, 300, 50.0), Some(0)),
            ],
        )
        .unwrap()
        .ramped(default);
        assert!(
            clipped[0] > clipped[1]
                && clipped[1] > clipped[2]
                && clipped[2] > clipped[3]
                && clipped[3] > 0.0,
            "{:?}",
            &clipped[..4]
        );
        assert_eq!(clipped[4], 0.0);
        assert_eq!(clipped[200], 50.0);
        // A curve built without overrides expands exactly as before.
        let plain = ShiftCurve::from_ramped_segments(
            2_200,
            &lone.iter().map(|item| (*item, None)).collect::<Vec<_>>(),
        )
        .unwrap()
        .ramped(default);
        assert_eq!(plain, shared);
    }

    /// Wall-clock budget as a multiple of the reference workload, so the
    /// assertion does not depend on how loaded the machine is. Measured at
    /// 4.6-6.1 units under the `perf` profile; the ceiling is twice the top.
    #[cfg(feature = "perf-tests")]
    const SHIFT_UNIT_BUDGET: f64 = 12.0;

    #[cfg(feature = "perf-tests")]
    #[test]
    fn cpu_budget_is_met_with_slack() {
        let unit = crate::test_support::reference_seconds();
        for rate in [44_100, 48_000, 96_000] {
            let tone = harmonic_tone(220.0, 1.0, rate);
            let input = stereo(&tone, &tone);
            let curve = ShiftCurve::constant(tone.len(), 100.0);
            // Best of three: the budget is a perf-profile ceiling, and a loaded
            // machine otherwise turns this into a flake.
            let mut best = f64::INFINITY;
            let mut frames = 0;
            for _ in 0..3 {
                let started = Instant::now();
                frames = shift(&input, 2, rate, &curve, &ShiftConfig::new(rate))
                    .unwrap()
                    .report
                    .frames;
                best = best.min(started.elapsed().as_secs_f64());
            }
            println!(
                "{rate} Hz stereo: {best:.4} s per audio second ({:.1}x realtime), {} frames",
                if best > 0.0 {
                    1.0 / best
                } else {
                    f64::INFINITY
                },
                frames
            );
            let ratio = best / unit;
            println!("{rate} Hz stereo: {ratio:.1}x the timing unit");
            assert!(
                ratio <= SHIFT_UNIT_BUDGET,
                "{rate} Hz used {best:.3} s per second, {ratio:.1}x the timing unit (budget {SHIFT_UNIT_BUDGET})"
            );
        }
    }

    fn steady_spectrum(samples: &[f32], rate: u32) -> (Vec<f64>, f64) {
        let head = (f64::from(rate) * 0.15) as usize;
        let tail = (f64::from(rate) * 0.15) as usize;
        spectrum(&samples[head..samples.len() - tail], rate)
    }

    fn shift_cents(input: &[f32], rate: u32, cents: f32, config: &ShiftConfig) -> ShiftOutput {
        let curve = ShiftCurve::constant(input.len(), cents);
        shift(input, 1, rate, &curve, config).unwrap()
    }

    fn peak_of(samples: &[f32]) -> f64 {
        samples
            .iter()
            .fold(0.0_f64, |worst, sample| worst.max(f64::from(sample.abs())))
    }

    #[test]
    #[ignore = "measurement harness; prints the formant tables for manual comparison"]
    fn formant_measurements_for_docs() {
        use std::time::Instant;

        let input = pulse_vowel(2.0, RATE, 140.0, 0.0015);
        let source = envelope_formants(&input, RATE);
        let clean = harmonic_vowel(200.0, 1.5, RATE);
        let (source_env, bin_hz) = long_envelope(&input, RATE);
        let low = (100.0 / bin_hz) as usize;
        let high = (5_000.0 / bin_hz) as usize;
        println!("source formants {source:?}");
        println!(
            "cents | F1 err % | F2 err % | LSD measured | LSD request | spur dBc | formant limited"
        );
        for cents in [
            -1_200.0_f32,
            -700.0,
            -300.0,
            -200.0,
            -100.0,
            100.0,
            200.0,
            300.0,
            700.0,
            1_200.0,
        ] {
            let ratio = 2f64.powf(f64::from(cents) / 1_200.0);
            let curve = ShiftCurve::constant_with_formant(input.len(), 0.0, cents);
            let output = shift(&input, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
            let got = envelope_formants(&output.samples, RATE);
            let errors: Vec<f64> = got
                .iter()
                .zip(&source)
                .map(|(value, before)| 100.0 * (value - before * ratio) / (before * ratio))
                .collect();
            let (shifted_env, _) = long_envelope(&output.samples, RATE);
            let last = source_env.len() - 1;
            let request: Vec<f64> = (0..source_env.len())
                .map(|bin| {
                    let read = bin as f64 / ratio;
                    if read >= last as f64 {
                        source_env[last]
                    } else {
                        let index = read.floor() as usize;
                        let fraction = read - index as f64;
                        source_env[index] + (source_env[index + 1] - source_env[index]) * fraction
                    }
                })
                .collect();
            let measured_lsd = lsd(&shifted_env, &source_env, low, high);
            let expected_lsd = lsd(&request, &source_env, low, high);
            let (magnitudes, bin_hz) = steady_spectrum(&output.samples, RATE);
            let spur = worst_spur_db(&magnitudes, bin_hz, 140.0);
            println!(
                "{cents:+6.0} | {:+.2} | {:+.2} | {:.2} | {:.2} | {:.1} | {}",
                errors[0],
                errors[1],
                measured_lsd,
                expected_lsd,
                spur,
                output.report.formant_limited_frames
            );
        }

        // Artefact budget on a strictly periodic source.
        for formant in [
            -1_200.0_f32,
            -700.0,
            -300.0,
            -100.0,
            100.0,
            300.0,
            700.0,
            1_200.0,
        ] {
            let curve = ShiftCurve::constant_with_formant(clean.len(), 0.0, formant);
            let output = shift(&clean, 1, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
            let (magnitudes, bin_hz) = steady_spectrum(&output.samples, RATE);
            let spur = worst_spur_db(&magnitudes, bin_hz, 200.0);
            let measured = measured_f0(
                &output.samples,
                f64::from(RATE),
                RATE as usize / 2,
                200.0,
                (RATE as f64 * 0.03) as usize,
            );
            println!(
                "clean 200 Hz {formant:+.0}: spur {spur:.1} dBc, pitch {:+.3} cents",
                cents(measured, 200.0)
            );
        }

        // CPU: formant-only versus pitch-only on one second of stereo audio.
        let mono = pulse_vowel(1.0, RATE, 140.0, 0.0015);
        let pair = stereo(&mono, &mono);
        for (label, cents, formant) in [
            ("pitch only", 100.0_f32, 0.0_f32),
            ("formant only", 0.0, 300.0),
            ("both", 100.0, 300.0),
        ] {
            let curve = ShiftCurve::constant_with_formant(pair.len() / 2, cents, formant);
            let mut best = f64::INFINITY;
            for _ in 0..3 {
                let started = Instant::now();
                let _ = shift(&pair, 2, RATE, &curve, &ShiftConfig::new(RATE)).unwrap();
                best = best.min(started.elapsed().as_secs_f64());
            }
            println!("{label}: {best:.4} s per stereo audio second");
        }
    }

    #[test]
    #[ignore = "measurement harness; prints the measurement tables for manual comparison"]
    fn measurements_for_docs() {
        let tone = harmonic_tone(220.0, 3.0, RATE);
        let target = 220.0 * 2f64.powf(100.0 / 1_200.0);
        let kilo = sine(1_000.0, 0.3, 1.5, RATE);
        let vib = vibrato_tone(220.0, 25.0, 6.0, 3.0, RATE);
        println!("scale | THD 1k | spur 220 | vib depth | f0 p95 | rms dB");
        for scale in [0.5_f32, 0.75, 1.0, 1.25, 1.5, 2.0] {
            let mut config = ShiftConfig::new(RATE);
            config.grain_scale = scale;
            let (magnitudes, bin_hz) =
                steady_spectrum(&shift_cents(&kilo, RATE, 100.0, &config).samples, RATE);
            let distortion = thd(&magnitudes, bin_hz, 1_000.0 * 2f64.powf(100.0 / 1_200.0));
            let (magnitudes, bin_hz) =
                steady_spectrum(&shift_cents(&tone, RATE, 100.0, &config).samples, RATE);
            let spur = worst_spur_db(&magnitudes, bin_hz, target);
            let track = pitch_track(
                &shift_cents(&tone, RATE, 100.0, &config).samples,
                RATE,
                target,
                0.2,
                0.2,
            );
            let p95 = percentile(&mut track.clone(), 0.95).abs();
            let shifted = shift_cents(&vib, RATE, 100.0, &config);
            let vib_track = pitch_track(&shifted.samples, RATE, target, 0.2, 0.2);
            let depth = (percentile(&mut vib_track.clone(), 0.95)
                - percentile(&mut vib_track.clone(), 0.05))
                / 2.0;
            let level = 20.0 * (rms(&shifted.samples) / rms(&vib)).log10();
            println!(
                "{scale:5.2} | {:.3} % | {spur:6.1} dBc | {depth:5.2} ct | {p95:5.3} ct | {level:+.2}",
                distortion * 100.0
            );
        }

        println!();
        println!("cents | f0 error | rms dB | peak dB | boundary step");
        for cents in [
            -1_200.0_f32,
            -1_000.0,
            -700.0,
            -400.0,
            -100.0,
            100.0,
            400.0,
            700.0,
            1_000.0,
            1_200.0,
        ] {
            let shifted = shift_cents(&tone, RATE, cents, &ShiftConfig::new(RATE));
            let expected = 220.0 * 2f64.powf(f64::from(cents) / 1_200.0);
            let error = cents_error_of(&shifted.samples, RATE, expected, 1.0);
            let head = (f64::from(RATE) * 0.2) as usize;
            let level = 20.0
                * (rms(&shifted.samples[head..shifted.samples.len() - head])
                    / rms(&tone[head..tone.len() - head]))
                .log10();
            let peak = 20.0 * (f64::from(shifted.report.peak) / peak_of(&tone)).log10();
            println!(
                "{cents:+6.0} | {error:+8.3} | {level:+6.2} | {peak:+7.2} | {:.3}",
                shifted.report.boundary_step_ratio
            );
        }

        println!();
        println!("rate | THD 1k | spur 220 | f0 p95");
        for rate in [22_050_u32, 44_100, 48_000, 96_000] {
            let kilo = sine(1_000.0, 0.3, 1.5, rate);
            let tone = harmonic_tone(220.0, 3.0, rate);
            let target = 220.0 * 2f64.powf(100.0 / 1_200.0);
            let config = ShiftConfig::new(rate);
            let kilo_shift = shift_cents(&kilo, rate, 100.0, &config);
            let (magnitudes, bin_hz) = steady_spectrum(&kilo_shift.samples, rate);
            let distortion = thd(&magnitudes, bin_hz, 1_000.0 * 2f64.powf(100.0 / 1_200.0));
            let tone_shift = shift_cents(&tone, rate, 100.0, &config);
            let (magnitudes, bin_hz) = steady_spectrum(&tone_shift.samples, rate);
            let spur = worst_spur_db(&magnitudes, bin_hz, target);
            let track = pitch_track(&tone_shift.samples, rate, target, 0.2, 0.2);
            let p95 = percentile(&mut track.clone(), 0.95).abs();
            println!(
                "{rate} | {:.3} % | {spur:6.1} dBc | {p95:5.3}",
                distortion * 100.0
            );
        }

        println!();
        println!("rate | mono s/s | stereo s/s");
        for rate in [44_100_u32, 48_000, 96_000] {
            let mono = harmonic_tone(220.0, 2.0, rate);
            let pair = stereo(&mono, &mono);
            let curve = ShiftCurve::constant(mono.len(), 100.0);
            let config = ShiftConfig::new(rate);
            let started = Instant::now();
            let _ = shift(&mono, 1, rate, &curve, &config).unwrap();
            let single = started.elapsed().as_secs_f64() / 2.0;
            let started = Instant::now();
            let _ = shift(&pair, 2, rate, &curve, &config).unwrap();
            let double = started.elapsed().as_secs_f64() / 2.0;
            println!("{rate} | {single:.4} | {double:.4}");
        }
    }

    // ---- note timing -----------------------------------------------------

    /// One 2 s take: the take's room (white noise at `room`) plus a tone.
    fn time_take(start_s: f64, seconds: f64, f0: f64, room: f64) -> Vec<f32> {
        let frames = 2 * RATE as usize;
        let tone = harmonic_tone(f0, seconds, RATE);
        let start = (start_s * f64::from(RATE)) as usize;
        let mut mono = white_noise(room, frames);
        for (index, value) in tone.iter().enumerate() {
            if start + index < frames {
                mono[start + index] += value;
            }
        }
        stereo(&mono, &mono)
    }

    fn time_frames(seconds: f64) -> usize {
        (seconds * f64::from(RATE)) as usize
    }

    /// First and last mono sample above `fraction` of the region's peak.
    fn audible_span(samples: &[f32], from: usize, to: usize, fraction: f64) -> (f64, f64) {
        let mono = channel(samples, 2, 0);
        let peak = mono[from..to]
            .iter()
            .fold(0.0_f64, |worst, sample| worst.max(f64::from(sample.abs())));
        let threshold = peak * fraction;
        let mut first = None;
        let mut last = None;
        for (index, sample) in mono.iter().enumerate().take(to).skip(from) {
            if f64::from(sample.abs()) >= threshold {
                first.get_or_insert(index);
                last = Some(index);
            }
        }
        (
            first.unwrap_or(from) as f64 / f64::from(RATE),
            last.unwrap_or(to) as f64 / f64::from(RATE),
        )
    }

    fn time_f0(samples: &[f32], at_s: f64, expected: f64) -> f64 {
        measured_f0(
            &channel(samples, 2, 0),
            f64::from(RATE),
            time_frames(at_s),
            expected,
            2_048,
        )
    }

    #[test]
    fn an_empty_or_noop_edit_list_is_bit_identical() {
        let buffer = time_take(0.6, 0.5, 220.0, 0.0);
        let config = TimeConfig::new(RATE);
        for edits in [
            Vec::new(),
            vec![TimeEdit::new(
                (time_frames(0.6), time_frames(1.1)),
                (time_frames(0.6), time_frames(1.1)),
            )],
        ] {
            let warped = time_warp(&buffer, 2, RATE, &edits, &config).unwrap();
            assert_eq!(warped.samples, buffer, "a no-op edit moved a sample");
            assert_eq!(warped.report.notes, 0);
        }
    }

    #[test]
    fn a_moved_note_lands_within_five_ms_and_keeps_its_pitch() {
        let buffer = time_take(0.6, 0.5, 220.0, 0.0);
        let input_span = audible_span(&buffer, time_frames(0.5), time_frames(1.2), 0.02);
        let config = TimeConfig::new(RATE);
        for move_s in [0.030_f64, -0.030] {
            let edit = TimeEdit::new(
                (time_frames(0.6), time_frames(1.1)),
                (time_frames(0.6 + move_s), time_frames(1.1 + move_s)),
            );
            let warped = time_warp(&buffer, 2, RATE, &[edit], &config).unwrap();
            assert_eq!(warped.samples.len(), buffer.len(), "length changed");
            let out_span = audible_span(
                &warped.samples,
                time_frames(0.4 + move_s.min(0.0)),
                time_frames(1.3 + move_s.max(0.0)),
                0.02,
            );
            let residual_ms = (out_span.0 - input_span.0 - move_s) * 1000.0;
            let pitch_error = cents(time_f0(&warped.samples, 0.6 + move_s + 0.15, 220.0), 220.0);
            println!(
                "move {move_s:+.3}s: onset {:.5} -> {:.5}, residual {residual_ms:+.2} ms, pitch {pitch_error:+.3} ct, boundary {:.3}x",
                input_span.0, out_span.0, warped.report.boundary_step_ratio
            );
            assert!(residual_ms.abs() <= 5.0, "residual {residual_ms:+.2} ms");
            assert!(pitch_error.abs() <= 2.0, "pitch {pitch_error:+.3} ct");
            assert!(
                warped.report.boundary_step_ratio <= 1.5,
                "{:.3}",
                warped.report.boundary_step_ratio
            );
        }
    }

    #[test]
    fn a_stretched_note_keeps_its_pitch_and_the_take_length() {
        let buffer = time_take(0.6, 0.4, 220.0, 0.0);
        let input_span = audible_span(&buffer, time_frames(0.5), time_frames(1.2), 0.02);
        let input_len = input_span.1 - input_span.0;
        let config = TimeConfig::new(RATE);
        for delta_s in [0.040_f64, -0.040] {
            let target_len = input_len + delta_s;
            let edit = TimeEdit::new(
                (time_frames(0.6), time_frames(1.0)),
                (time_frames(0.6), time_frames(0.6) + time_frames(target_len)),
            );
            let warped = time_warp(&buffer, 2, RATE, &[edit], &config).unwrap();
            assert_eq!(warped.samples.len(), buffer.len(), "take length changed");
            let out_span = audible_span(&warped.samples, time_frames(0.5), time_frames(1.2), 0.02);
            let out_len = out_span.1 - out_span.0;
            let length_error_ms = (out_len - target_len) * 1000.0;
            let pitch_error = cents(
                time_f0(&warped.samples, 0.6 + target_len * 0.5, 220.0),
                220.0,
            );
            println!(
                "length {delta_s:+.3}s: {input_len:.4} -> {out_len:.4} (requested {target_len:.4}), error {length_error_ms:+.2} ms, pitch {pitch_error:+.3} ct, boundary {:.3}x",
                warped.report.boundary_step_ratio
            );
            assert!(
                length_error_ms.abs() <= 3.0,
                "length error {length_error_ms:+.2} ms"
            );
            assert!(pitch_error.abs() <= 2.0, "pitch {pitch_error:+.3} ct");
            assert!(
                warped.report.boundary_step_ratio <= 1.5,
                "{:.3}",
                warped.report.boundary_step_ratio
            );
        }
    }

    #[test]
    fn two_adjacent_edited_notes_do_not_click_at_the_shared_edge() {
        // One continuous take: the note change itself has no input transient.
        let sequence = sequence(&[(220.0, 0.4), (330.0, 0.4)], 0.3, RATE);
        let mut mono = vec![0.0_f32; 2 * RATE as usize];
        let offset = time_frames(0.6);
        mono[offset..offset + sequence.len()].copy_from_slice(&sequence);
        let buffer = stereo(&mono, &mono);
        let edits = [
            TimeEdit::new(
                (time_frames(0.6), time_frames(1.0)),
                (time_frames(0.62), time_frames(1.02)),
            ),
            TimeEdit::new(
                (time_frames(1.0), time_frames(1.4)),
                (time_frames(1.02), time_frames(1.42)),
            ),
        ];
        let warped = time_warp(&buffer, 2, RATE, &edits, &TimeConfig::new(RATE)).unwrap();
        println!(
            "adjacent moves: seam {:.3}x, pitch {:.3} ct",
            warped.report.boundary_step_ratio,
            cents(time_f0(&warped.samples, 0.62 + 0.15, 220.0), 220.0)
        );
        assert!(
            warped.report.boundary_step_ratio <= 1.5,
            "{:.3}",
            warped.report.boundary_step_ratio
        );
        assert!(cents(time_f0(&warped.samples, 0.62 + 0.15, 220.0), 220.0).abs() <= 2.0);
        assert!(cents(time_f0(&warped.samples, 1.02 + 0.15, 330.0), 330.0).abs() <= 2.0);
        // The 20 ms gap that opened in front of the first note is real room,
        // not the note's own first period.
        let lead = rms(&channel(&warped.samples, 2, 0)[time_frames(0.6)..time_frames(0.618)]);
        println!("lead level {lead:.6}");
        assert!(
            lead < 1e-4,
            "the vacated 20 ms still holds the note: {lead}"
        );
    }

    #[test]
    fn only_the_edited_span_moves() {
        let buffer = time_take(0.6, 0.5, 220.0, 0.0);
        let edit = TimeEdit::new(
            (time_frames(0.6), time_frames(1.1)),
            (time_frames(0.63), time_frames(1.13)),
        );
        let warped = time_warp(&buffer, 2, RATE, &[edit], &TimeConfig::new(RATE)).unwrap();
        for (from, to) in [(0.0, 0.59), (1.16, 2.0)] {
            let range = time_frames(from) * 2..time_frames(to) * 2;
            assert_eq!(
                &warped.samples[range.clone()],
                &buffer[range],
                "an untouched span moved"
            );
        }
    }

    #[test]
    fn overlapping_edits_and_out_of_range_edits_are_errors() {
        let buffer = time_take(0.6, 0.5, 220.0, 0.0);
        let config = TimeConfig::new(RATE);
        let a = TimeEdit::new(
            (time_frames(0.2), time_frames(0.5)),
            (time_frames(0.2), time_frames(0.5)),
        );
        let b = TimeEdit::new(
            (time_frames(0.4), time_frames(0.7)),
            (time_frames(1.2), time_frames(1.5)),
        );
        assert_eq!(
            time_warp(&buffer, 2, RATE, &[a, b], &config).unwrap_err(),
            ShiftError::OverlappingEdits
        );
        let c = TimeEdit::new(
            (time_frames(0.2), time_frames(0.5)),
            (time_frames(0.4), time_frames(0.7)),
        );
        let d = TimeEdit::new(
            (time_frames(0.6), time_frames(0.9)),
            (time_frames(0.5), time_frames(0.8)),
        );
        assert_eq!(
            time_warp(&buffer, 2, RATE, &[c, d], &config).unwrap_err(),
            ShiftError::OverlappingEdits
        );
        let outside = TimeEdit::new(
            (time_frames(0.2), time_frames(0.5)),
            (time_frames(1.9), time_frames(2.2)),
        );
        assert_eq!(
            time_warp(&buffer, 2, RATE, &[outside], &config).unwrap_err(),
            ShiftError::EditOutOfRange
        );
        let empty = TimeEdit::new(
            (time_frames(0.2), time_frames(0.2)),
            (time_frames(0.4), time_frames(0.5)),
        );
        assert_eq!(
            time_warp(&buffer, 2, RATE, &[empty], &config).unwrap_err(),
            ShiftError::EmptyEdit
        );
    }

    #[test]
    fn the_gap_keeps_the_take_room_instead_of_digital_silence() {
        let buffer = time_take(0.6, 0.5, 220.0, 0.01);
        let room = rms(&channel(&buffer, 2, 0)[0..time_frames(0.4)]);
        let edit = TimeEdit::new(
            (time_frames(0.6), time_frames(1.1)),
            (time_frames(0.63), time_frames(1.13)),
        );
        let warped = time_warp(&buffer, 2, RATE, &[edit], &TimeConfig::new(RATE)).unwrap();
        let gap = channel(&warped.samples, 2, 0);
        // The vacated head of the note is 30 ms; skip its crossfades.
        let measured = rms(&gap[time_frames(0.605)..time_frames(0.625)]);
        let ratio_db = 20.0 * (measured / room).log10();
        println!(
            "room {room:.5}, gap {measured:.5} ({ratio_db:+.2} dB), room frames {}",
            warped.report.room_frames
        );
        assert!(
            ratio_db.abs() <= 3.0,
            "gap {ratio_db:+.2} dB away from room"
        );
        assert!(measured > 0.0, "gap is digital silence");
    }
}
