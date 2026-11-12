//! Note-model resolution and offline baking for `@vocal_tune`.
//!
//! The project stores only the human's decisions; this module matches them to
//! the derived `.ocn` analysis, resolves targets to frequencies and bakes the
//! playback window through the offline pitch shifter. Offline/worker path only;
//! the audio callback never calls this. See `docs/vocal-tune.md` for the model,
//! the resolution rules and the bake.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use oc_dsl::ast::{
    AudioClip, KeySignature, NoteName, Project, StretchMode, VocalSnapPolicy, VocalTarget,
    VocalTune, VocalTuneNote, VocalVibratoMode,
};
use oc_dsl::timing::ProjectClock;
use oc_dsp::vocal_analysis::{VocalFrame, VocalNote};
use oc_dsp::vocal_shift::{
    self, ShiftConfig, ShiftCurve, ShiftReport, ShiftSegment, TimeConfig, TimeEdit, TimeReport,
};

use crate::vocal_analysis::{ClipAnalysis, VocalAnalysisKey};
use crate::vocal_snap::{self, SnapConfig, SnapOutcome};

/// Bump when any change here can move a baked sample.
///
/// Composed with [`vocal_shift::ALGO_VERSION`] in [`BakeKey`], so a change to
/// the shifter alone also invalidates an older bake. 3 added automatic targets
/// (scale snap and reference matching) and formant displacement; 4 added
/// note-level timing (move and length change); 5 adds vibrato editing.
pub const BAKE_VERSION: u32 = 5;
/// A stored anchor this close to a detected onset is the same note.
pub const ANCHOR_TOLERANCE_SECONDS: f64 = 0.030;
/// Notes below this confidence are left untouched and reported.
pub const CONFIDENCE_FLOOR: f64 = 0.5;
/// A resolved target this far from the detected pitch is warned about.
pub const OCTAVE_WARNING_CENTS: f64 = 1_200.0;
/// Measured vibrato shallower than this is not worth resynthesising, in cents.
pub const VIBRATO_FLOOR_CENTS: f64 = 1.0;
/// Depth a `scale` note keeps; the grammar carries no per-note factor.
pub const VIBRATO_SCALE_FACTOR: f64 = 0.5;
/// Contour resolution: the analyzer hop is 5 ms, and the shifter tracks a
/// 1 ms contour without stepping at the grain rate.
pub const VIBRATO_CONTOUR_HOP_SECONDS: f64 = 0.001;

/// Bake algorithm identity as reported and used in paths: `<bake>.<shifter>`.
pub fn version_stamp() -> String {
    format!("{BAKE_VERSION}.{}", vocal_shift::ALGO_VERSION)
}

/// What the resolver decided for one stored override or automatic proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteOutcome {
    /// A stored target resolved to a non-zero shift.
    Shift,
    /// Resolved to zero shift: no audio change, whatever the reason.
    PassThrough,
    /// No target stored and the automatic pass is off, so nothing aims the note.
    NoTarget,
    /// An automatic target was chosen; it may still resolve to zero.
    Snapped,
    /// Already within the snap tolerance: the samples are not touched.
    InTune,
    /// The nearest automatic target is beyond the cap; left untouched.
    Capped,
    /// No scale is active at the note's position; left untouched.
    NoKey,
    /// The matched note's confidence is below the floor; left untouched.
    LowConfidence,
    /// The anchor no longer matches a detected onset; left untouched.
    StaleAnchor,
    /// The note carries `bypass=true`.
    Bypassed,
    /// Only a vibrato edit moves this note; its pitch target did not.
    Vibrato,
}

impl NoteOutcome {
    /// Wire label for the outcome.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shift => "shift",
            Self::PassThrough => "pass-through",
            Self::NoTarget => "no-target",
            Self::Snapped => "snapped",
            Self::InTune => "in-tune",
            Self::Capped => "capped",
            Self::NoKey => "no-key",
            Self::LowConfidence => "low-confidence",
            Self::StaleAnchor => "stale-anchor",
            Self::Bypassed => "bypassed",
            Self::Vibrato => "vibrato",
        }
    }
}

/// The measured contour a vibrato edit applies on top of a note's pitch shift.
#[derive(Debug, Clone, PartialEq)]
pub struct VibratoEdit {
    pub mode: VocalVibratoMode,
    /// Measured input depth: half the 5th..95th percentile spread, in cents.
    pub depth_cents: f64,
    /// Contour step in seconds (1 ms after interpolating the analysis frames).
    pub hop_seconds: f64,
    /// Clip-local time of `cents[0]`.
    pub start_seconds: f64,
    /// Extra shift per contour point, in cents; never empty here.
    pub cents: Vec<f32>,
}

/// One override resolved against the analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedNote {
    /// Stored anchor in clip-local seconds.
    pub anchor_s: f64,
    pub detected_index: Option<usize>,
    pub detected_start_s: Option<f64>,
    pub detected_hz: Option<f64>,
    pub detected_confidence: Option<f64>,
    /// Absolute target in MIDI cents when the target could be resolved.
    pub target_cents: Option<f64>,
    /// Effective correction in percent (0..=100).
    pub amount: f64,
    /// `amount / 100 * (target - detected)`, in cents.
    pub shift_cents: Option<f64>,
    /// Effective formant displacement in cents: the note override, else the
    /// block default, clamped to the shifter's honest range.
    pub formant_cents: f64,
    pub resulting_hz: Option<f64>,
    /// Stored per-note glide override; `None` keeps the block default.
    pub retune_seconds: Option<f64>,
    /// Stored onset move in seconds relative to the detected onset.
    pub start_seconds: Option<f64>,
    /// Stored length multiplier for the detected span.
    pub length: Option<f64>,
    /// Effective vibrato policy: the note override, else the block default.
    pub vibrato_mode: VocalVibratoMode,
    /// Measured vibrato contour when the policy edits this note.
    pub vibrato: Option<VibratoEdit>,
    pub outcome: NoteOutcome,
    pub warnings: Vec<String>,
}

/// One note's timing decision after resolution, in seconds of the take.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTiming {
    pub detected_index: usize,
    /// Stored anchor, matched against the detected onset.
    pub anchor_s: f64,
    /// Start of the reported span this edit moves, on the frame grid.
    pub detected_start_s: f64,
    /// Length of the reported span this edit moves.
    pub detected_length_s: f64,
    /// Stored move, echoed.
    pub move_seconds: f64,
    /// Length the note asked for: the detected length times the stored ratio.
    pub requested_length_s: f64,
    /// The note's span before and after the edit, in playback frames.
    pub source: (usize, usize),
    pub target: (usize, usize),
    /// Filled by a bake: what the output actually measures.
    pub measured: Option<TimingMeasurement>,
}

/// Audible result of one timing edit, measured on the baked window.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TimingMeasurement {
    /// Measured onset of the baked note minus the requested onset, in ms.
    pub onset_residual_ms: f64,
    /// Baked span length minus the requested span length, in ms.
    pub length_error_ms: f64,
    /// Measured audible length minus the input's audible length scaled by the
    /// requested ratio, in ms. This is the content-scaling error; the span
    /// itself also carries the analyser's margins.
    pub audible_length_error_ms: f64,
    /// First and last sample above the measurement threshold, in seconds.
    pub audible_start_s: f64,
    pub audible_length_s: f64,
}

/// One tune block resolved against its clip's analysis.
#[derive(Debug, Clone)]
pub struct ResolvedTune {
    pub tune: String,
    pub clip: String,
    pub track: String,
    pub sample_rate: u32,
    pub analysis_key: VocalAnalysisKey,
    pub analysis_from_cache: bool,
    /// The automatic policy this pass ran, after `auto` picked by `reference`.
    pub snap: VocalSnapPolicy,
    /// Reference clip the automatic pass followed, when it had one.
    pub reference: Option<String>,
    /// Stored overrides, in file order.
    pub notes: Vec<ResolvedNote>,
    /// Automatic proposals for detected notes without a stored override.
    pub proposals: Vec<ResolvedNote>,
    /// Proposals that took their target from the reference clip.
    pub reference_notes: usize,
    /// Proposals with no reference note that used the scale instead.
    pub reference_fallbacks: usize,
    /// Detected notes with no stored override.
    pub unoverridden_notes: usize,
    /// Resolved timing edits, one per note that carries `start=`/`length=`.
    pub timing: Vec<ResolvedTiming>,
    pub warnings: Vec<String>,
}

impl ResolvedTune {
    /// Notes and proposals in one time order.
    pub fn all_notes(&self) -> Vec<&ResolvedNote> {
        let mut all: Vec<(usize, &ResolvedNote)> = self
            .notes
            .iter()
            .chain(&self.proposals)
            .filter_map(|note| note.detected_index.map(|index| (index, note)))
            .collect();
        all.sort_by_key(|(index, _)| *index);
        all.into_iter().map(|(_, note)| note).collect()
    }

    /// Decisions that would move audio.
    pub fn shifted_notes(&self) -> usize {
        self.notes
            .iter()
            .chain(&self.proposals)
            .filter(|note| {
                note.shift_cents.is_some_and(|cents| cents != 0.0)
                    || note.formant_cents != 0.0
                    || note.vibrato.is_some()
            })
            .count()
    }

    /// Notes a timing edit moves or resizes.
    pub fn timing_notes(&self) -> usize {
        self.timing.len()
    }

    /// Largest shift across notes and proposals.
    pub fn max_shift_cents(&self) -> f64 {
        self.notes
            .iter()
            .chain(&self.proposals)
            .filter_map(|note| {
                let base = note.shift_cents.unwrap_or(0.0).abs();
                let extra = note.vibrato.as_ref().map_or(0.0, |edit| {
                    edit.cents
                        .iter()
                        .fold(0.0_f32, |worst, cents| worst.max(cents.abs()))
                        as f64
                });
                (note.shift_cents.is_some() || note.vibrato.is_some()).then_some(base + extra)
            })
            .fold(0.0_f64, f64::max)
    }

    /// Nothing moves pitch, envelope, vibrato or time: the bake is a copy.
    pub fn pass_through(&self) -> bool {
        self.shifted_notes() == 0 && self.timing.is_empty()
    }
}

/// Baked media and where the output project should read it.
#[derive(Debug, Clone)]
pub struct BakedMedia {
    pub path: PathBuf,
    /// Path relative to the output project, as stored in `file:`.
    pub file: String,
    pub frames: usize,
    pub sample_rate: u32,
    pub from_cache: bool,
}

/// Result of one `vocal apply` run.
#[derive(Debug, Clone)]
pub struct AppliedTune {
    pub resolved: ResolvedTune,
    pub baked: Option<BakedMedia>,
    /// What the shifter did; `None` for a dry run or a silent curve.
    pub report: Option<ShiftReport>,
    /// What the time pass did; `None` when no note carried a timing edit.
    pub time: Option<TimeReport>,
    /// Written project, or `None` for a dry run.
    pub project_out: Option<PathBuf>,
}

/// MIDI cents for a frequency: C-1 is 0 and A4 is 6900.
pub fn hz_to_midi_cents(hz: f64) -> f64 {
    6_900.0 + 1_200.0 * (hz / 440.0).log2()
}

/// Frequency for MIDI cents on the same C-1 = 0 grid.
pub fn midi_cents_to_hz(cents: f64) -> f64 {
    440.0 * 2.0_f64.powf((cents - 6_900.0) / 1_200.0)
}

/// MIDI note of a one-based scale degree in the tonic's octave.
///
/// Octave numbers follow pitch names: C4 is MIDI 60, so the tonic of
/// `A major` in octave 4 is A4 (69) and degree 3 is C#5 (73).
pub fn scale_degree_midi(scale: &KeySignature, degree: u8, octave: i32) -> Result<u8, String> {
    if !(1..=7).contains(&degree) {
        return Err(format!("scale degree {degree} must be 1..=7"));
    }
    let tonic = vocal_snap::pitch_class(scale.root, scale.accidental);
    let midi =
        (octave + 1) * 12 + tonic + vocal_snap::scale_steps(scale.mode)[usize::from(degree - 1)];
    if !(0..=127).contains(&midi) {
        return Err(format!(
            "scale degree {degree}@{octave} is outside MIDI 0..=127"
        ));
    }
    Ok(midi as u8)
}

/// Absolute target in MIDI cents for one target spec.
pub fn resolve_target_cents(
    target: &VocalTarget,
    detected_cents: f64,
    scale: Option<&KeySignature>,
) -> Result<f64, String> {
    Ok(match target {
        VocalTarget::Pitch(midi) => f64::from(*midi) * 100.0,
        VocalTarget::Cents(cents) => *cents,
        VocalTarget::Semitones(semitones) => detected_cents + semitones * 100.0,
        VocalTarget::OffsetCents(cents) => detected_cents + cents,
        VocalTarget::Degree { degree, octave } => {
            let scale = scale.ok_or_else(|| {
                "a scale-degree target needs a scale; set scale: or @project key:".to_string()
            })?;
            f64::from(scale_degree_midi(scale, *degree, *octave)?) * 100.0
        }
    })
}

/// Index of the detected note whose onset is within tolerance of `anchor_s`.
pub fn match_note(notes: &[VocalNote], anchor_s: f64) -> Option<usize> {
    let mut best: Option<(f64, usize)> = None;
    for (index, note) in notes.iter().enumerate() {
        let distance = (note.onset_s - anchor_s).abs();
        if distance <= ANCHOR_TOLERANCE_SECONDS && best.is_none_or(|(best, _)| distance < best) {
            best = Some((distance, index));
        }
    }
    best.map(|(_, index)| index)
}

fn nearest_onset(notes: &[VocalNote], anchor_s: f64) -> Option<(f64, f64)> {
    notes
        .iter()
        .map(|note| ((note.onset_s - anchor_s).abs(), note.onset_s))
        .min_by(|left, right| left.0.total_cmp(&right.0))
}

/// Resolve one override against its matched note. Pure; the CLI and tests use it.
pub fn resolve_override(
    note: &VocalTuneNote,
    detected: &VocalNote,
    scale: Option<&KeySignature>,
    default_amount: f64,
    confidence_floor: f64,
) -> Result<ResolvedNote, String> {
    let amount = note.amount.unwrap_or(default_amount);
    let mut resolved = ResolvedNote {
        anchor_s: note.at,
        detected_index: None,
        detected_start_s: Some(detected.onset_s),
        detected_hz: Some(detected.f0_median_hz),
        detected_confidence: Some(detected.confidence),
        target_cents: None,
        amount,
        shift_cents: None,
        formant_cents: 0.0,
        resulting_hz: None,
        retune_seconds: note.retune_seconds,
        start_seconds: note.start_seconds,
        length: note.length,
        vibrato_mode: note.vibrato.unwrap_or(VocalVibratoMode::Preserve),
        vibrato: None,
        outcome: NoteOutcome::NoTarget,
        warnings: Vec::new(),
    };
    if note.bypass {
        resolved.outcome = NoteOutcome::Bypassed;
        return Ok(resolved);
    }
    let Some(target) = note.target else {
        return Ok(resolved);
    };
    let detected_cents = hz_to_midi_cents(detected.f0_median_hz);
    let target_cents = resolve_target_cents(&target, detected_cents, scale)?;
    resolved.target_cents = Some(target_cents);
    if detected.confidence < confidence_floor {
        resolved.outcome = NoteOutcome::LowConfidence;
        resolved.warnings.push(format!(
            "confidence {:.2} is below the {:.2} floor; left untouched",
            detected.confidence, confidence_floor
        ));
        return Ok(resolved);
    }
    let raw_shift = target_cents - detected_cents;
    if raw_shift.abs() > OCTAVE_WARNING_CENTS {
        resolved.warnings.push(format!(
            "target is {:.0} cents from the detected pitch (more than an octave)",
            raw_shift
        ));
    }
    let shift_cents = amount / 100.0 * raw_shift;
    if shift_cents.abs() > f64::from(vocal_shift::MAX_SHIFT_CENTS) {
        resolved.warnings.push(format!(
            "shift of {shift_cents:+.0} cents is beyond the ±{:.0} the shifter is designed for and is clamped",
            vocal_shift::MAX_SHIFT_CENTS
        ));
    }
    resolved.shift_cents = Some(shift_cents);
    resolved.resulting_hz = Some(midi_cents_to_hz(detected_cents + shift_cents));
    resolved.outcome = if shift_cents == 0.0 {
        NoteOutcome::PassThrough
    } else {
        NoteOutcome::Shift
    };
    Ok(resolved)
}

/// The reference clip a block names, when it is still in the project.
pub fn reference_clip<'a>(project: &'a Project, tune: &VocalTune) -> Option<&'a AudioClip> {
    let name = tune.reference.as_deref()?;
    project
        .audio_clips()
        .into_iter()
        .find(|clip| clip.name == name)
}

/// The tune block bound to a clip, if any.
pub fn tune_for_clip<'a>(project: &'a Project, clip: &AudioClip) -> Option<&'a VocalTune> {
    project.properties.vocal_tunes.iter().find(|tune| {
        tune.clip.as_deref() == Some(clip.name.as_str())
            || (tune.clip.is_none() && tune.track.as_deref() == Some(clip.track.as_str()))
    })
}

/// Frames of glide for one note: its own override, else the block default.
fn retune_frames(tune: &VocalTune, note: &ResolvedNote, sample_rate: u32) -> usize {
    let seconds = note.retune_seconds.unwrap_or(tune.retune_seconds).max(0.0);
    (seconds * f64::from(sample_rate)).round() as usize
}

/// The resolved shifts as one curve over the clip's detected note spans.
///
/// Each note covers exactly its destination span, so a bypassed, stale,
/// low-confidence or unoverridden note contributes no segment and its samples
/// are never touched. A vibrato edit subdivides the span at its contour
/// points; every other note is one segment carrying its glide, so per-note
/// `retune` works without a second shifter call.
pub fn shift_curve(
    frames: usize,
    sample_rate: u32,
    tune: &VocalTune,
    resolved: &ResolvedTune,
    notes: &[VocalNote],
) -> Result<ShiftCurve, String> {
    let rate = f64::from(sample_rate);
    let all = resolved.all_notes();
    let mut segments: Vec<(ShiftSegment, Option<usize>)> = Vec::with_capacity(all.len());
    let mut cursor = 0;
    for note in all {
        let formant = note.formant_cents;
        let cents = note.shift_cents.unwrap_or(0.0);
        if cents == 0.0 && formant == 0.0 && note.vibrato.is_none() {
            continue;
        }
        let Some(index) = note.detected_index else {
            continue;
        };
        let Some(detected) = notes.get(index) else {
            continue;
        };
        let source_start = ((detected.start_s * rate).round().max(0.0) as usize).max(cursor);
        let source_end = ((detected.end_s * rate).round().max(0.0) as usize).min(frames);
        if source_end <= source_start + 1 {
            continue;
        }
        // A timing edit moves the note, so its pitch segment follows it to the
        // destination span; a note without one keeps its detected span.
        let (start, end) = resolved
            .timing
            .iter()
            .find(|entry| entry.detected_index == index)
            .map_or((source_start, source_end), |entry| {
                (entry.target.0.min(frames), entry.target.1.min(frames))
            });
        let start = start.max(cursor);
        if end <= start + 1 {
            continue;
        }
        cursor = end;
        let retune = retune_frames(tune, note, sample_rate);
        match &note.vibrato {
            Some(edit) => push_vibrato_segments(
                &mut segments,
                start,
                end,
                cents as f32,
                formant as f32,
                edit,
                retune,
                sample_rate,
            ),
            None => segments.push((
                ShiftSegment {
                    start,
                    end,
                    cents: cents as f32,
                    formant_cents: formant as f32,
                },
                Some(retune),
            )),
        }
    }
    ShiftCurve::from_ramped_segments(frames, &segments)
        .map_err(|error| format!("pitch shift curve: {error}"))
}

/// Split a vibrato-edited note into one segment per contour point.
///
/// The contour tiles the destination span, so a stretched note keeps its
/// vibrato shape. `retune` becomes an envelope over the note edges; interior
/// segments use a one-hop slew so the contour is smooth without rounding off
/// the measured depth.
#[allow(clippy::too_many_arguments)]
fn push_vibrato_segments(
    segments: &mut Vec<(ShiftSegment, Option<usize>)>,
    start: usize,
    end: usize,
    base: f32,
    formant: f32,
    edit: &VibratoEdit,
    retune: usize,
    sample_rate: u32,
) {
    let count = edit.cents.len();
    if count == 0 || end <= start + 1 {
        return;
    }
    let hop = (edit.hop_seconds * f64::from(sample_rate)).round().max(1.0) as usize;
    let span = (end - start) as f64;
    for index in 0..count {
        let begin = start + ((index as f64 * span / count as f64).round() as usize);
        let finish = start + (((index + 1) as f64 * span / count as f64).round() as usize);
        if finish <= begin {
            continue;
        }
        let center = 0.5 * (begin + finish) as f64;
        let gain = if retune == 0 {
            1.0
        } else {
            let distance = (center - start as f64).min(end as f64 - center);
            0.5 - 0.5 * (std::f64::consts::PI * (distance / retune as f64).min(1.0)).cos()
        };
        let cents = (f64::from(base) + f64::from(edit.cents[index])) * gain;
        let formant_cents = f64::from(formant) * gain;
        if cents == 0.0 && formant_cents == 0.0 {
            continue;
        }
        segments.push((
            ShiftSegment {
                start: begin,
                end: finish,
                cents: cents as f32,
                formant_cents: formant_cents as f32,
            },
            Some(hop),
        ));
    }
}

/// Baked window plus what the passes did.
#[derive(Debug, Clone, PartialEq)]
pub struct ShiftOutcome {
    pub samples: Vec<f32>,
    pub report: ShiftReport,
    /// Time-pass report; `None` when no note carries a timing edit.
    pub time: Option<TimeReport>,
}

/// Apply the resolved decisions to one interleaved stereo playback window.
///
/// The time pass runs first (only when a note carries a timing edit) and the
/// pitch shifter then resynthesises the destination spans, so a note can be
/// both retuned and moved. A resolution with no shift, formant or vibrato is
/// the shifter's own zero-cents path and stays bit-identical to `window`.
pub fn shift_window(
    sample_rate: u32,
    window: &[f32],
    tune: &VocalTune,
    resolved: &ResolvedTune,
    notes: &[VocalNote],
) -> Result<ShiftOutcome, String> {
    let frames = window.len() / 2;
    if let Some(entry) = resolved
        .timing
        .iter()
        .find(|entry| entry.source.1 > frames || entry.target.1 > frames)
    {
        return Err(format!(
            "the playback window is {frames} frames, shorter than the timing edit at {:.3}s expects",
            entry.anchor_s
        ));
    }
    let warped: Vec<f32>;
    let mut time = None;
    let input: &[f32] = if resolved.timing.is_empty() {
        window
    } else {
        let edits: Vec<TimeEdit> = resolved
            .timing
            .iter()
            .map(|entry| TimeEdit::new(entry.source, entry.target))
            .collect();
        let output = vocal_shift::time_warp(
            window,
            2,
            sample_rate,
            &edits,
            &TimeConfig::new(sample_rate),
        )
        .map_err(|error| format!("note timing failed: {error}"))?;
        warped = output.samples;
        time = Some(output.report);
        &warped
    };
    let curve = shift_curve(frames, sample_rate, tune, resolved, notes)?;
    let mut config = ShiftConfig::new(sample_rate);
    config.ramp_frames = (tune.retune_seconds.max(0.0) * f64::from(sample_rate)).round() as usize;
    let output = vocal_shift::shift(input, 2, sample_rate, &curve, &config)
        .map_err(|error| format!("pitch shift failed: {error}"))?;
    Ok(ShiftOutcome {
        samples: output.samples,
        report: output.report,
        time,
    })
}

/// First and last frame above 2 % of the window's peak, all channels.
fn audible_window(
    samples: &[f32],
    frames: usize,
    channels: usize,
    from: usize,
    to: usize,
) -> Option<(usize, usize)> {
    let from = from.min(frames);
    let to = to.min(frames);
    if to <= from {
        return None;
    }
    let level = |frame: usize| {
        (0..channels).fold(0.0_f32, |worst, channel| {
            worst.max(samples[frame * channels + channel].abs())
        })
    };
    let peak = (from..to).fold(0.0_f32, |worst, frame| worst.max(level(frame)));
    if peak < 1e-5 {
        return None;
    }
    let threshold = peak * 0.02;
    let mut first = None;
    let mut last = None;
    for frame in from..to {
        if level(frame) >= threshold {
            first.get_or_insert(frame);
            last = Some(frame);
        }
    }
    Some((first?, last?))
}

/// Measure each resolved timing edit on the baked window.
///
/// The onset oracle is an energy threshold, not the pitch detector: it answers
/// "when does the note start" from the output alone, so the residual is not the
/// resolver grading itself. `length` is measured between the same threshold
/// crossings around the destination span.
pub fn measure_timing(
    window: &[f32],
    baked: &[f32],
    sample_rate: u32,
    resolved: &mut ResolvedTune,
) {
    let rate = f64::from(sample_rate);
    let frames = baked.len() / 2;
    let search = (rate * 0.030).max(1.0) as usize;
    for entry in &mut resolved.timing {
        let source = entry.source;
        let Some((in_start, _)) = audible_window(
            window,
            frames,
            2,
            source.0.saturating_sub(search),
            (source.0 + search).min(frames),
        ) else {
            continue;
        };
        let Some((_, in_end)) = audible_window(
            window,
            frames,
            2,
            source.1.saturating_sub(search),
            (source.1 + search).min(frames),
        ) else {
            continue;
        };
        let target = entry.target;
        let Some((out_start, _)) = audible_window(
            baked,
            frames,
            2,
            target.0.saturating_sub(search),
            (target.0 + search).min(frames),
        ) else {
            continue;
        };
        let Some((_, out_end)) = audible_window(
            baked,
            frames,
            2,
            target.1.saturating_sub(search),
            (target.1 + search).min(frames),
        ) else {
            continue;
        };
        let requested_onset = in_start as f64 + entry.move_seconds * rate;
        let measured_length = (out_end as f64 - out_start as f64).max(0.0);
        let ratio = entry.requested_length_s / entry.detected_length_s.max(1e-9);
        let expected_length = (in_end as f64 - in_start as f64) * ratio;
        entry.measured = Some(TimingMeasurement {
            onset_residual_ms: (out_start as f64 - requested_onset) / rate * 1000.0,
            length_error_ms: ((target.1 - target.0) as f64 - entry.requested_length_s * rate)
                / rate
                * 1000.0,
            audible_length_error_ms: (measured_length - expected_length) / rate * 1000.0,
            audible_start_s: out_start as f64 / rate,
            audible_length_s: measured_length / rate,
        });
    }
}

/// Effective formant cents for one override, clamped to the honest range.
///
/// The DSP can reshape up to ±1200 cents, but the acceptance table stops
/// holding above ±300 (LPC F1/F2 error, then the sideband budget); larger
/// requests are clamped here so a bake cannot pretend to a vocal-tract move the
/// envelope cannot deliver.
fn effective_formant(tune: &VocalTune, note: &VocalTuneNote) -> (f64, Option<String>) {
    let requested = note.formant_cents.unwrap_or(tune.formant_cents);
    let cap = f64::from(vocal_shift::HONEST_FORMANT_CENTS);
    if requested.abs() > cap {
        (
            requested.clamp(-cap, cap),
            Some(format!(
                "formant shift of {requested:+.0} cents is beyond the ±{cap:.0} the envelope reshape keeps honest and is clamped"
            )),
        )
    } else {
        (requested, None)
    }
}

pub(crate) fn key_at(
    project: &Project,
    clock: &ProjectClock,
    timeline_s: f64,
) -> Result<Option<KeySignature>, String> {
    let quarter = clock.quarter_at_seconds(timeline_s)?;
    let mut best: Option<(f64, KeySignature)> = None;
    for point in &project.properties.key_map {
        let point_quarter = clock.quarter_notes_at(&point.position)?;
        if point_quarter <= quarter + 1e-9
            && best.is_none_or(|(best_quarter, _)| point_quarter >= best_quarter)
        {
            best = Some((point_quarter, point.key));
        }
    }
    Ok(best.map(|(_, key)| key).or(project.properties.key))
}

/// The key active for one detected note: the block scale, else `key_map` at
/// the note's timeline position, else the project key.
fn scale_at(
    project: &Project,
    clock: &ProjectClock,
    tune: &VocalTune,
    analysis: &ClipAnalysis,
    note: &VocalNote,
) -> Result<Option<KeySignature>, String> {
    if tune.scale.is_some() {
        return Ok(tune.scale);
    }
    key_at(project, clock, analysis.clip_start_s + note.onset_s)
}

/// Correction for one resolved target and the frequency it produces.
fn correction(detected_cents: f64, target_cents: f64, amount: f64) -> (f64, f64) {
    let shift_cents = amount / 100.0 * (target_cents - detected_cents);
    (shift_cents, midi_cents_to_hz(detected_cents + shift_cents))
}

/// Match every stored override to the analysis and resolve its target.
pub fn resolve_tune(
    project: &Project,
    clip: &AudioClip,
    tune: &VocalTune,
    analysis: &ClipAnalysis,
) -> Result<ResolvedTune, String> {
    resolve_tune_with_reference(project, clip, tune, analysis, None)
}

/// [`resolve_tune`] plus the reference clip's analysis for `snap: reference`.
///
/// Stored per-note targets are matched first and are final. The automatic pass
/// then fills every detected note the block did not target itself, in this
/// precedence: `bypass` → low confidence → stored target → reference note →
/// scale. See `docs/vocal-tune.md`.
pub fn resolve_tune_with_reference(
    project: &Project,
    clip: &AudioClip,
    tune: &VocalTune,
    analysis: &ClipAnalysis,
    reference: Option<&ClipAnalysis>,
) -> Result<ResolvedTune, String> {
    let clock = ProjectClock::new(&project.properties)?;
    let notes = &analysis.analysis.notes;
    let policy = vocal_snap::resolved_policy(tune);
    let mut resolved = ResolvedTune {
        tune: tune.name.clone(),
        clip: clip.name.clone(),
        track: clip.track.clone(),
        sample_rate: analysis.target_rate,
        analysis_key: analysis.key.clone(),
        analysis_from_cache: analysis.from_cache,
        snap: policy,
        reference: tune.reference.clone(),
        notes: Vec::with_capacity(tune.notes.len()),
        proposals: Vec::new(),
        reference_notes: 0,
        reference_fallbacks: 0,
        unoverridden_notes: 0,
        timing: Vec::new(),
        warnings: Vec::new(),
    };
    let mut claimed: Vec<Option<usize>> = vec![None; notes.len()];
    for (index, note) in tune.notes.iter().enumerate() {
        let Some(detected_index) = match_note(notes, note.at) else {
            let nearest = nearest_onset(notes, note.at);
            let mut item = ResolvedNote {
                anchor_s: note.at,
                detected_index: None,
                detected_start_s: None,
                detected_hz: None,
                detected_confidence: None,
                target_cents: None,
                amount: note.amount.unwrap_or(tune.amount),
                shift_cents: None,
                formant_cents: 0.0,
                resulting_hz: None,
                retune_seconds: note.retune_seconds,
                start_seconds: note.start_seconds,
                length: note.length,
                vibrato_mode: note.vibrato.unwrap_or(tune.vibrato),
                vibrato: None,
                outcome: NoteOutcome::StaleAnchor,
                warnings: Vec::new(),
            };
            item.warnings.push(match nearest {
                Some((distance, start)) => format!(
                    "anchor {:.3}s no longer matches a detected onset (nearest {:.3}s, {:.0} ms away); left untouched",
                    note.at,
                    start,
                    distance * 1000.0
                ),
                None => format!(
                    "anchor {:.3}s no longer matches a detected onset (the clip has no notes); left untouched",
                    note.at
                ),
            });
            resolved.notes.push(item);
            continue;
        };
        if let Some(first) = claimed[detected_index] {
            let first_anchor = tune.notes[first].at;
            return Err(format!(
                "@vocal_tune '{}': anchors {first_anchor:.3}s and {:.3}s resolve to the same detected note at {:.3}s; there is no merge rule, so store one override per note",
                tune.name, note.at, notes[detected_index].onset_s
            ));
        }
        claimed[detected_index] = Some(index);
        let timeline_s = analysis.clip_start_s + note.at;
        let scale = if tune.scale.is_some() {
            tune.scale
        } else {
            key_at(project, &clock, timeline_s)?
        };
        let mut item = resolve_override(
            note,
            &notes[detected_index],
            scale.as_ref(),
            tune.amount,
            CONFIDENCE_FLOOR,
        )
        .map_err(|error| format!("@vocal_tune '{}': note {index}: {error}", tune.name))?;
        item.detected_index = Some(detected_index);
        item.vibrato_mode = note.vibrato.unwrap_or(tune.vibrato);
        let detected = &notes[detected_index];
        if !matches!(item.outcome, NoteOutcome::Bypassed) {
            if item.outcome == NoteOutcome::LowConfidence {
                // Already reported as untouched; the envelope follows.
            } else if detected.confidence < CONFIDENCE_FLOOR {
                item.outcome = NoteOutcome::LowConfidence;
                item.warnings.push(format!(
                    "confidence {:.2} is below the {:.2} floor; left untouched",
                    detected.confidence, CONFIDENCE_FLOOR
                ));
            } else {
                let (formant, warning) = effective_formant(tune, note);
                if let Some(warning) = warning {
                    item.warnings.push(warning);
                }
                item.formant_cents = formant;
                if formant != 0.0
                    && matches!(
                        item.outcome,
                        NoteOutcome::NoTarget | NoteOutcome::PassThrough
                    )
                {
                    item.outcome = NoteOutcome::Shift;
                }
            }
        }
        resolved.notes.push(item);
    }
    resolved.unoverridden_notes = notes
        .iter()
        .enumerate()
        .filter(|(index, _)| claimed[*index].is_none())
        .count();

    let automatic = policy != VocalSnapPolicy::Off && tune.amount > 0.0;
    if automatic {
        resolve_automatic(
            AutomaticTuneContext {
                project,
                clock: &clock,
                tune,
                analysis,
                reference,
                policy,
            },
            &claimed,
            &mut resolved,
        )?;
    }
    resolved.timing = resolve_timing(tune, analysis, &resolved)?;
    resolve_vibrato(tune, analysis, &mut resolved);
    Ok(resolved)
}

struct AutomaticTuneContext<'a> {
    project: &'a Project,
    clock: &'a ProjectClock,
    tune: &'a VocalTune,
    analysis: &'a ClipAnalysis,
    reference: Option<&'a ClipAnalysis>,
    policy: VocalSnapPolicy,
}

/// Reference notes take precedence over scale snapping with contour.
fn resolve_automatic(
    context: AutomaticTuneContext<'_>,
    claimed: &[Option<usize>],
    resolved: &mut ResolvedTune,
) -> Result<(), String> {
    let AutomaticTuneContext {
        project,
        clock,
        tune,
        analysis,
        reference,
        policy,
    } = context;
    let notes = &analysis.analysis.notes;
    let config = SnapConfig::from_tune(tune);
    if policy == VocalSnapPolicy::Reference && reference.is_none() {
        resolved.warnings.push(format!(
            "reference '{}' was not analysed; stored targets and the scale are used instead",
            tune.reference.as_deref().unwrap_or("")
        ));
    }
    if let Some(reference) = reference {
        let take = (
            analysis.clip_start_s,
            analysis.clip_start_s + analysis.duration_s,
        );
        let other = (
            reference.clip_start_s,
            reference.clip_start_s + reference.duration_s,
        );
        if take.1 <= other.0 || other.1 <= take.0 {
            resolved.warnings.push(format!(
                "reference '{}' ({:.3}-{:.3}s) does not overlap the take ({:.3}-{:.3}s); the scale is used instead",
                tune.reference.as_deref().unwrap_or(""),
                other.0,
                other.1,
                take.0,
                take.1
            ));
        }
    }

    // Provisional targets for the contour lookahead: a stored target is final,
    // an automatic note is represented by its nearest scale pitch.
    let mut provisional: Vec<Option<f64>> = vec![None; notes.len()];
    for (index, detected) in notes.iter().enumerate() {
        let stored = claimed[index].map(|stored| &resolved.notes[stored]);
        if let Some(item) = stored
            && item.target_cents.is_some()
            && matches!(item.outcome, NoteOutcome::Shift | NoteOutcome::PassThrough)
        {
            provisional[index] = item.target_cents;
            continue;
        }
        let skipped = stored.is_some_and(|item| {
            matches!(
                item.outcome,
                NoteOutcome::Bypassed | NoteOutcome::LowConfidence | NoteOutcome::StaleAnchor
            )
        });
        if skipped || detected.confidence < CONFIDENCE_FLOOR {
            continue;
        }
        let scale = scale_at(project, clock, tune, analysis, detected)?;
        provisional[index] = scale.and_then(|scale| {
            // A note that cannot move must not steer its neighbour's contour,
            // so a capped note has no provisional target either.
            let proposal = vocal_snap::snap_note(
                hz_to_midi_cents(detected.f0_median_hz),
                &vocal_snap::scale_pitch_classes(&scale),
                config,
                None,
                None,
                None,
                None,
            );
            proposal
                .target_cents
                .filter(|_| proposal.outcome != SnapOutcome::Capped)
        });
    }

    let mut prev_detected: Option<f64> = None;
    let mut prev_chosen: Option<f64> = None;
    let mut low_confidence = 0;
    let mut no_key: Option<f64> = None;
    for (index, detected) in notes.iter().enumerate() {
        let detected_cents = hz_to_midi_cents(detected.f0_median_hz);
        let mut next_detected = None;
        let mut next_chosen = None;
        if index + 1 < notes.len()
            && let Some(chosen) = provisional[index + 1]
        {
            next_detected = Some(hz_to_midi_cents(notes[index + 1].f0_median_hz));
            next_chosen = Some(chosen);
        }
        let stored = claimed[index];
        if let Some(stored) = stored {
            let item = &resolved.notes[stored];
            let settled = item.target_cents.is_some()
                && matches!(item.outcome, NoteOutcome::Shift | NoteOutcome::PassThrough);
            if settled {
                prev_detected = Some(detected_cents);
                prev_chosen = item.target_cents;
                continue;
            }
            if matches!(
                item.outcome,
                NoteOutcome::Bypassed | NoteOutcome::LowConfidence | NoteOutcome::StaleAnchor
            ) {
                continue;
            }
        }
        if detected.confidence < CONFIDENCE_FLOOR {
            low_confidence += 1;
            match stored {
                Some(stored) => resolved.notes[stored].outcome = NoteOutcome::LowConfidence,
                None => {
                    let mut item = automatic_note(detected, index, tune.amount, tune.vibrato);
                    item.outcome = NoteOutcome::LowConfidence;
                    resolved.proposals.push(item);
                }
            }
            continue;
        }

        let mut target_cents = None;
        let mut from_reference = false;
        if policy == VocalSnapPolicy::Reference
            && let Some(reference) = reference
        {
            let local_s = analysis.clip_start_s + detected.onset_s - reference.clip_start_s;
            let matched = vocal_snap::reference_note(
                &reference.analysis.notes,
                local_s,
                ANCHOR_TOLERANCE_SECONDS,
            )
            .filter(|note| note.confidence >= CONFIDENCE_FLOOR);
            if let Some(reference_note) = matched {
                let folded = vocal_snap::fold_octaves(
                    hz_to_midi_cents(reference_note.f0_median_hz),
                    detected_cents,
                );
                target_cents = Some(folded);
                from_reference = true;
            } else {
                resolved.reference_fallbacks += 1;
            }
        }
        let mut outcome = NoteOutcome::Snapped;
        let mut warnings = Vec::new();
        if let Some(target) = target_cents {
            if (target - detected_cents).abs() <= config.tolerance_cents {
                outcome = NoteOutcome::InTune;
            } else if (target - detected_cents).abs() > config.cap_cents {
                warnings.push(format!(
                    "reference target is {:.0} cents from the detected pitch; the cap only bounds scale snap",
                    target - detected_cents
                ));
            }
            resolved.reference_notes += usize::from(from_reference);
        } else {
            match scale_at(project, clock, tune, analysis, detected)? {
                Some(scale) => {
                    let mask = vocal_snap::scale_pitch_classes(&scale);
                    let proposal = vocal_snap::snap_note(
                        detected_cents,
                        &mask,
                        config,
                        prev_detected,
                        prev_chosen,
                        next_detected,
                        next_chosen,
                    );
                    target_cents = proposal.target_cents;
                    outcome = match proposal.outcome {
                        SnapOutcome::InTune => NoteOutcome::InTune,
                        SnapOutcome::Snapped => NoteOutcome::Snapped,
                        SnapOutcome::Capped => NoteOutcome::Capped,
                    };
                }
                None => {
                    outcome = NoteOutcome::NoKey;
                    no_key.get_or_insert(detected.onset_s);
                }
            }
        }

        let amount = match stored {
            Some(stored) => resolved.notes[stored].amount,
            None => tune.amount,
        };
        let mut item = match stored {
            Some(stored) => resolved.notes[stored].clone(),
            None => automatic_note(detected, index, amount, tune.vibrato),
        };
        item.target_cents = target_cents;
        item.outcome = outcome;
        item.warnings.extend(warnings);
        match outcome {
            NoteOutcome::InTune => {
                item.shift_cents = Some(0.0);
                item.resulting_hz = Some(detected.f0_median_hz);
            }
            NoteOutcome::Snapped => {
                if let Some(target) = target_cents {
                    let (shift, hz) = correction(detected_cents, target, amount);
                    item.shift_cents = Some(shift);
                    item.resulting_hz = Some(hz);
                }
            }
            _ => {}
        }
        match stored {
            Some(stored) => resolved.notes[stored] = item,
            None => resolved.proposals.push(item),
        }
        if matches!(outcome, NoteOutcome::Snapped | NoteOutcome::InTune) && target_cents.is_some() {
            prev_detected = Some(detected_cents);
            prev_chosen = target_cents;
        }
    }
    if let Some(start) = no_key {
        resolved.warnings.push(format!(
            "no key at {start:.3}s: add scale: or @project key: to snap automatically"
        ));
    }
    if low_confidence > 0 {
        resolved.warnings.push(format!(
            "{low_confidence} low-confidence note(s) were left untouched"
        ));
    }
    if policy == VocalSnapPolicy::Reference {
        let total = resolved.reference_notes + resolved.reference_fallbacks;
        if resolved.reference_fallbacks > 0 {
            resolved.warnings.push(format!(
                "reference '{}': {} of {total} automatic note(s) had no reference note and used the scale",
                tune.reference.as_deref().unwrap_or(""),
                resolved.reference_fallbacks
            ));
        }
    }
    Ok(())
}

/// Fill every matched note's vibrato contour from the analysis frames.
///
/// The block default applies unless the note carries `vibrato=`. A bypassed,
/// stale or low-confidence note is left alone, exactly as for a pitch target.
/// A note whose measured depth is below [`VIBRATO_FLOOR_CENTS`] keeps its
/// samples untouched and is not reported as a vibrato edit.
fn resolve_vibrato(tune: &VocalTune, analysis: &ClipAnalysis, resolved: &mut ResolvedTune) {
    let frames = &analysis.analysis.frames;
    let hop_seconds = analysis.analysis.hop_seconds;
    let mut represented = vec![false; analysis.analysis.notes.len()];
    let mut edited = 0_usize;
    let mut requested = tune.vibrato != VocalVibratoMode::Preserve;
    for note in resolved
        .notes
        .iter_mut()
        .chain(resolved.proposals.iter_mut())
    {
        if let Some(index) = note.detected_index
            && let Some(slot) = represented.get_mut(index)
        {
            *slot = true;
        }
        if note.vibrato_mode == VocalVibratoMode::Preserve {
            continue;
        }
        requested = true;
        if matches!(
            note.outcome,
            NoteOutcome::Bypassed | NoteOutcome::LowConfidence | NoteOutcome::StaleAnchor
        ) {
            continue;
        }
        let Some(index) = note.detected_index else {
            continue;
        };
        let Some(detected) = analysis.analysis.notes.get(index) else {
            continue;
        };
        let Some(edit) = measure_vibrato(frames, hop_seconds, detected, note.vibrato_mode) else {
            continue;
        };
        if note.shift_cents.is_none_or(|cents| cents == 0.0) && note.formant_cents == 0.0 {
            note.outcome = NoteOutcome::Vibrato;
        }
        note.vibrato = Some(edit);
        edited += 1;
    }
    // With `snap: off` there is no automatic proposal pass, but the block
    // default still edits the vibrato of every detected note.
    if tune.vibrato != VocalVibratoMode::Preserve {
        for (index, detected) in analysis.analysis.notes.iter().enumerate() {
            if represented[index] || detected.confidence < CONFIDENCE_FLOOR {
                continue;
            }
            let Some(edit) = measure_vibrato(frames, hop_seconds, detected, tune.vibrato) else {
                continue;
            };
            let mut item = automatic_note(detected, index, tune.amount, tune.vibrato);
            item.outcome = NoteOutcome::Vibrato;
            item.vibrato = Some(edit);
            resolved.proposals.push(item);
            edited += 1;
        }
    }
    if requested && edited == 0 {
        resolved.warnings.push(format!(
            "vibrato: no matched note could be edited (low confidence or a depth below {VIBRATO_FLOOR_CENTS:.0} ct); the take was left alone"
        ));
    }
}

/// Measure one note's vibrato and turn it into the extra shift per frame.
///
/// `flatten` targets the note's own median, `scale` targets
/// `median + 0.5 * (detected - median)`; both are measured from the analyzer's
/// per-frame f0, never from a fitted rate or depth. Frames the detector did not
/// voice are filled by linear interpolation so the curve has no holes.
fn measure_vibrato(
    frames: &[VocalFrame],
    hop_seconds: f64,
    detected: &VocalNote,
    mode: VocalVibratoMode,
) -> Option<VibratoEdit> {
    let from = frames.partition_point(|frame| frame.time_s < detected.start_s);
    let to = frames.partition_point(|frame| frame.time_s < detected.end_s);
    if to <= from + 1 {
        return None;
    }
    let slice = &frames[from..to];
    let median_cents = hz_to_midi_cents(detected.f0_median_hz);
    let deviation: Vec<Option<f64>> = slice
        .iter()
        .map(|frame| {
            (frame.voiced && frame.confidence >= CONFIDENCE_FLOOR)
                .then(|| hz_to_midi_cents(frame.f0_hz) - median_cents)
        })
        .collect();
    let measured: Vec<f64> = deviation.iter().filter_map(|value| *value).collect();
    if measured.len() < 2 {
        return None;
    }
    let depth_cents = half_amplitude(measured);
    if depth_cents < VIBRATO_FLOOR_CENTS {
        return None;
    }
    let deviation = fill_gaps(&deviation);
    let kept = if mode == VocalVibratoMode::Scale {
        1.0 - VIBRATO_SCALE_FACTOR
    } else {
        1.0
    };
    // Interpolate the per-frame track onto a 1 ms grid: the shifter reads the
    // contour at each grain, so a 5 ms step would add its own modulation.
    let length = detected.end_s - detected.start_s;
    let count = (length / VIBRATO_CONTOUR_HOP_SECONDS).ceil().max(1.0) as usize;
    let step = length / count as f64;
    let first = slice[0].time_s;
    let cents: Vec<f32> = (0..count)
        .map(|index| {
            let time = detected.start_s + (index as f64 + 0.5) * step;
            let position = (time - first) / hop_seconds;
            -sample_track(&deviation, position) * kept as f32
        })
        .collect();
    Some(VibratoEdit {
        mode,
        depth_cents,
        hop_seconds: step,
        start_seconds: detected.start_s,
        cents,
    })
}

/// Linear interpolation of a per-frame track at a fractional frame position.
fn sample_track(values: &[f64], position: f64) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let clamped = position.clamp(0.0, (values.len() - 1) as f64);
    let low = clamped.floor() as usize;
    let high = (low + 1).min(values.len() - 1);
    let fraction = clamped - low as f64;
    (values[low] + (values[high] - values[low]) * fraction) as f32
}

/// Half the 5th..95th percentile spread, a robust peak amplitude.
fn half_amplitude(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let at = |quantile: f64| {
        let index = ((values.len() - 1) as f64 * quantile).round() as usize;
        values[index]
    };
    (at(0.95) - at(0.05)) * 0.5
}

/// Linear interpolation across unvoiced or low-confidence frames.
fn fill_gaps(values: &[Option<f64>]) -> Vec<f64> {
    let mut out = vec![0.0_f64; values.len()];
    let mut previous: Option<(usize, f64)> = None;
    for (index, value) in values.iter().enumerate() {
        let Some(value) = value else {
            continue;
        };
        match previous {
            Some((previous_index, previous_value)) => {
                let span = (index - previous_index) as f64;
                for (gap, sample) in out
                    .iter_mut()
                    .enumerate()
                    .take(index)
                    .skip(previous_index + 1)
                {
                    let weight = (gap - previous_index) as f64 / span;
                    *sample = previous_value + (value - previous_value) * weight;
                }
            }
            None => out[..index].fill(*value),
        }
        out[index] = *value;
        previous = Some((index, *value));
    }
    if let Some((index, value)) = previous {
        out[index + 1..].fill(value);
    }
    out
}

/// Resolve every stored timing field and check where the notes land.
///
/// Source spans are clamped exactly the way [`shift_curve`] clamps them, so the
/// pitch curve and the time pass agree on a note's span. A timing field on an
/// override that matched no detected note is an error, as is a move or a length
/// change beyond the block `max_move`, a destination outside the take, and a
/// destination that lands on another note. Exact no-ops are dropped, which is
/// what keeps a zero request bit-identical.
fn resolve_timing(
    tune: &VocalTune,
    analysis: &ClipAnalysis,
    resolved: &ResolvedTune,
) -> Result<Vec<ResolvedTiming>, String> {
    let rate = f64::from(analysis.target_rate);
    let frames = (analysis.duration_s * rate).round().max(0.0) as usize;
    let notes = &analysis.analysis.notes;
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(notes.len());
    let mut cursor = 0_usize;
    for note in notes {
        let start = ((note.start_s * rate).round().max(0.0) as usize)
            .max(cursor)
            .min(frames);
        let end = (note.end_s * rate).round().max(0.0) as usize;
        let end = end.min(frames).max(start);
        spans.push((start, end));
        cursor = end;
    }
    let cap = tune.max_move_seconds.max(0.0);
    let mut timing: Vec<ResolvedTiming> = Vec::new();
    for (index, note) in tune.notes.iter().enumerate() {
        if note.start_seconds.is_none() && note.length.is_none() {
            continue;
        }
        let Some(item) = resolved.notes.get(index) else {
            continue;
        };
        if item.outcome == NoteOutcome::Bypassed {
            continue;
        }
        let Some(detected_index) = item.detected_index else {
            return Err(format!(
                "@vocal_tune '{}': note {index}: timing fields need a detected note, and anchor {:.3}s matches none",
                tune.name, note.at
            ));
        };
        let detected = &notes[detected_index];
        let (source_start, source_end) = spans[detected_index];
        let move_seconds = note.start_seconds.unwrap_or(0.0);
        if move_seconds.abs() > cap + 1e-9 {
            return Err(format!(
                "@vocal_tune '{}': anchor {:.3}s: a move of {:.0} ms is beyond the ±{:.0} ms max_move",
                tune.name,
                note.at,
                move_seconds * 1000.0,
                cap * 1000.0
            ));
        }
        let ratio = note.length.unwrap_or(1.0);
        let source_len = (source_end - source_start) as f64;
        let requested_len = source_len * ratio;
        let target_len = requested_len.round().max(1.0) as isize;
        let length_change = (target_len as f64 - source_len) / rate;
        if length_change.abs() > cap + 1e-9 {
            return Err(format!(
                "@vocal_tune '{}': anchor {:.3}s: a length of {:.0} ms changes the note by {:+.0} ms, beyond the ±{:.0} ms max_move",
                tune.name,
                note.at,
                requested_len / rate * 1000.0,
                length_change * 1000.0,
                cap * 1000.0
            ));
        }
        let move_frames = (move_seconds * rate).round() as isize;
        let target_start = source_start as isize + move_frames;
        let target_end = target_start + target_len;
        if target_start < 0 {
            return Err(format!(
                "@vocal_tune '{}': anchor {:.3}s: moving {:.0} ms earlier leaves the take by {:.0} ms",
                tune.name,
                note.at,
                move_seconds * 1000.0,
                -(target_start as f64) / rate * 1000.0
            ));
        }
        if target_end > frames as isize {
            return Err(format!(
                "@vocal_tune '{}': anchor {:.3}s: moving {:.0} ms later runs past the end of the take by {:.0} ms",
                tune.name,
                note.at,
                move_seconds * 1000.0,
                (target_end - frames as isize) as f64 / rate * 1000.0
            ));
        }
        let entry = ResolvedTiming {
            detected_index,
            anchor_s: note.at,
            detected_start_s: detected.start_s,
            detected_length_s: source_len / rate,
            move_seconds,
            requested_length_s: requested_len / rate,
            source: (source_start, source_end),
            target: (target_start as usize, target_end as usize),
            measured: None,
        };
        if entry.source != entry.target {
            timing.push(entry);
        }
    }
    timing.sort_by_key(|entry| entry.detected_index);

    // Where every detected note lands: an edited note's target, every other
    // note's own span. Only a collision that involves an edit matters here; the
    // analyser's spans are already disjoint.
    let mut destinations: Vec<(usize, usize, usize)> = spans
        .iter()
        .enumerate()
        .filter(|(_, (start, end))| end > start)
        .map(|(index, (start, end))| (*start, *end, index))
        .collect();
    for entry in &timing {
        if let Some(slot) = destinations
            .iter_mut()
            .find(|(_, _, index)| *index == entry.detected_index)
        {
            slot.0 = entry.target.0;
            slot.1 = entry.target.1;
        }
    }
    destinations.sort_by_key(|(start, _, _)| *start);
    for pair in destinations.windows(2) {
        if pair[0].1 <= pair[1].0 {
            continue;
        }
        let involved = timing
            .iter()
            .any(|entry| entry.detected_index == pair[0].2 || entry.detected_index == pair[1].2);
        if !involved {
            continue;
        }
        return Err(format!(
            "@vocal_tune '{}': notes at {:.3}s and {:.3}s would overlap by {:.0} ms after the timing edits; move one less or shorten it",
            tune.name,
            notes[pair[0].2].onset_s,
            notes[pair[1].2].onset_s,
            (pair[0].1 - pair[1].0) as f64 / rate * 1000.0
        ));
    }
    Ok(timing)
}

/// A proposal for one detected note that carries no stored decisions.
fn automatic_note(
    detected: &VocalNote,
    index: usize,
    amount: f64,
    vibrato: VocalVibratoMode,
) -> ResolvedNote {
    ResolvedNote {
        anchor_s: detected.onset_s,
        detected_index: Some(index),
        detected_start_s: Some(detected.onset_s),
        detected_hz: Some(detected.f0_median_hz),
        detected_confidence: Some(detected.confidence),
        target_cents: None,
        amount,
        shift_cents: None,
        formant_cents: 0.0,
        resulting_hz: None,
        retune_seconds: None,
        start_seconds: None,
        length: None,
        vibrato_mode: vibrato,
        vibrato: None,
        outcome: NoteOutcome::NoTarget,
        warnings: Vec::new(),
    }
}

/// Canonical string of the decisions that can move baked samples.
pub fn decision_stamp(tune: &VocalTune) -> String {
    let mut out = format!(
        "amount={:016x};retune={:016x};formant={:016x};vibrato={};scale={};snap={};reference={};cap={:016x};window={:016x};max_move={:016x}",
        tune.amount.to_bits(),
        tune.retune_seconds.to_bits(),
        tune.formant_cents.to_bits(),
        tune.vibrato.as_str(),
        tune.scale.map_or_else(
            || "inherit".to_string(),
            |scale| format!(
                "{}:{:?}:{:?}",
                scale_note_name(scale.root),
                scale.accidental,
                scale.mode
            )
        ),
        tune.snap.as_str(),
        tune.reference.as_deref().unwrap_or("none"),
        tune.snap_cap_cents.to_bits(),
        tune.snap_window_cents.to_bits(),
        tune.max_move_seconds.to_bits(),
    );
    for note in &tune.notes {
        out.push_str(&format!(
            ";at={:016x};target={};amount={};retune={};formant={};start={};length={};vibrato={};bypass={}",
            note.at.to_bits(),
            note.target
                .as_ref()
                .map_or_else(|| "none".to_string(), target_stamp),
            option_bits(note.amount),
            option_bits(note.retune_seconds),
            option_bits(note.formant_cents),
            option_bits(note.start_seconds),
            option_bits(note.length),
            note.vibrato.map_or("inherit", |mode| mode.as_str()),
            note.bypass
        ));
    }
    out
}

fn option_bits(value: Option<f64>) -> String {
    value.map_or_else(
        || "none".to_string(),
        |value| format!("{:016x}", value.to_bits()),
    )
}

fn target_stamp(target: &VocalTarget) -> String {
    match target {
        VocalTarget::Pitch(midi) => format!("pitch:{midi}"),
        VocalTarget::Cents(cents) => format!("cents:{:016x}", cents.to_bits()),
        VocalTarget::Semitones(semitones) => format!("semitones:{:016x}", semitones.to_bits()),
        VocalTarget::OffsetCents(cents) => format!("offset:{:016x}", cents.to_bits()),
        VocalTarget::Degree { degree, octave } => format!("degree:{degree}:{octave}"),
    }
}

fn scale_note_name(note: NoteName) -> &'static str {
    match note {
        NoteName::C => "C",
        NoteName::D => "D",
        NoteName::E => "E",
        NoteName::F => "F",
        NoteName::G => "G",
        NoteName::A => "A",
        NoteName::B => "B",
    }
}

/// Identity of one baked buffer: analysis identity plus the decisions.
///
/// `BAKE_VERSION` also appears in the directory name, so a version bump never
/// reads an older buffer even before the key is compared.
#[derive(Debug, Clone)]
pub struct BakeKey {
    pub analysis: VocalAnalysisKey,
    pub decisions: u64,
    /// Hash of the reference clip's analysis key, when the block snaps to one.
    pub reference: Option<u64>,
    /// Bake algorithm version.
    pub version: u32,
    /// Shifter algorithm version: a shifter change alone must miss.
    pub shifter: u32,
}

impl BakeKey {
    /// `reference` is the reference clip's analysis key when the block has one:
    /// a changed reference take has to invalidate the bake too.
    pub fn new(
        analysis: VocalAnalysisKey,
        tune: &VocalTune,
        reference: Option<&VocalAnalysisKey>,
    ) -> Self {
        Self::with_versions(
            analysis,
            tune,
            reference,
            BAKE_VERSION,
            vocal_shift::ALGO_VERSION,
        )
    }

    /// Key for an analysis and tune at explicit versions.
    pub fn with_versions(
        analysis: VocalAnalysisKey,
        tune: &VocalTune,
        reference: Option<&VocalAnalysisKey>,
        version: u32,
        shifter: u32,
    ) -> Self {
        Self {
            analysis,
            decisions: oc_dsp::vocal_analysis::fnv1a(decision_stamp(tune).as_bytes()),
            reference: reference.map(|key| oc_dsp::vocal_analysis::fnv1a(key.stamp().as_bytes())),
            version,
            shifter,
        }
    }

    /// Stable key string.
    pub fn stamp(&self) -> String {
        let reference = self
            .reference
            .map_or_else(|| "none".to_string(), |hash| format!("{hash:016x}"));
        format!(
            "{}|{:016x}|{reference}|{}.{}",
            self.analysis.stamp(),
            self.decisions,
            self.version,
            self.shifter
        )
    }

    /// Cache file name for a clip.
    pub fn file_name(&self, clip: &str) -> String {
        format!(
            "{}-{:016x}.wav",
            sanitize_file_stem(clip),
            oc_dsp::vocal_analysis::fnv1a(self.stamp().as_bytes())
        )
    }

    /// Versioned cache directory name.
    pub fn dir_name(&self) -> String {
        format!("v{}.{}", self.version, self.shifter)
    }
}

pub(crate) fn sanitize_file_stem(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("clip");
    }
    out
}

/// Write the baked window as 32-bit float stereo, atomically.
pub(crate) fn write_baked_wav(
    path: &Path,
    samples: &[f32],
    sample_rate: u32,
) -> Result<(), String> {
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let write = || -> Result<(), String> {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&temp, spec)
            .map_err(|error| format!("{}: {error}", temp.display()))?;
        for sample in samples {
            writer
                .write_sample(*sample)
                .map_err(|error| error.to_string())?;
        }
        writer.finalize().map_err(|error| error.to_string())?;
        fs::rename(&temp, path).map_err(|error| format!("{}: {error}", path.display()))
    };
    write().inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

/// Read a cached bake when its header matches exactly; a mismatch is a miss.
pub(crate) fn read_baked(path: &Path, frames: usize, sample_rate: u32) -> Option<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    if spec.channels != 2
        || spec.sample_rate != sample_rate
        || spec.bits_per_sample != 32
        || spec.sample_format != hound::SampleFormat::Float
        || reader.duration() as usize != frames
    {
        return None;
    }
    reader
        .samples::<f32>()
        .collect::<Result<Vec<f32>, _>>()
        .ok()
}

/// Directory for render-time bakes, next to the derived analysis cache.
fn shared_bake_path(key: &BakeKey, clip: &str) -> Option<PathBuf> {
    crate::vocal_analysis::default_cache_dir().map(|dir| {
        dir.join("bake")
            .join(key.dir_name())
            .join(key.file_name(clip))
    })
}

/// The clip a block names as its reference, plus the analysis key it bakes
/// against. `None` when the block has no reference.
///
/// Computing the key reads the reference's playback window, so a reference
/// block pays one extra decode per render; the analysis itself is cached.
fn reference_key(
    project: &Project,
    tune: &VocalTune,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<Option<(usize, VocalAnalysisKey)>, String> {
    let Some(name) = &tune.reference else {
        return Ok(None);
    };
    let Some(index) = project
        .audio_clips()
        .iter()
        .position(|clip| &clip.name == name)
    else {
        return Err(format!(
            "@vocal_tune '{}': reference clip '{name}' not found",
            tune.name
        ));
    };
    let clip = project.audio_clip(index).unwrap();
    let (key, _) = crate::vocal_analysis::clip_stereo_playback(project, clip, sample_rate, root)?;
    Ok(Some((index, key)))
}

/// Realised automatic targets: the reference analysis when the block asks for
/// one. The caller resolves it once and passes it to
/// [`resolve_tune_with_reference`].
fn analyse_reference(
    project: &Project,
    tune: &VocalTune,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<Option<ClipAnalysis>, String> {
    let Some(name) = &tune.reference else {
        return Ok(None);
    };
    let Some(clip) = project
        .audio_clips()
        .into_iter()
        .find(|clip| &clip.name == name)
    else {
        return Ok(None);
    };
    crate::vocal_analysis::analyze_clip(
        project,
        clip,
        sample_rate,
        root,
        &oc_dsp::vocal_analysis::DetectorParams::default(),
        None,
    )
    .map(Some)
}

/// Baked window for the render path, or `None` when the clip has no block.
///
/// A cache hit skips the analysis and the shift, so a second render of the same
/// decisions is free. Cache writes are best effort: a read-only cache directory
/// must not fail a render.
pub fn render_window(
    project: &Project,
    clip: &AudioClip,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<Option<Vec<f32>>, String> {
    Ok(render_window_with_key(project, clip, sample_rate, root)?.map(|(_, samples)| samples))
}

/// [`render_window`] plus the analysis key the bake was made from.
///
/// A harmony voice reads its notes from the same window, so it takes both from
/// here instead of decoding the clip a second time.
pub(crate) fn render_window_with_key(
    project: &Project,
    clip: &AudioClip,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<Option<(VocalAnalysisKey, Vec<f32>)>, String> {
    let Some(tune) = tune_for_clip(project, clip) else {
        return Ok(None);
    };
    let (key, window) =
        crate::vocal_analysis::clip_stereo_playback(project, clip, sample_rate, root)?;
    let reference = reference_key(project, tune, sample_rate, root)?;
    let bake_key = BakeKey::new(key.clone(), tune, reference.as_ref().map(|(_, key)| key));
    let frames = window.len() / 2;
    let cached = shared_bake_path(&bake_key, &clip.name);
    if let Some(path) = &cached
        && let Some(samples) = read_baked(path, frames, sample_rate)
    {
        return Ok(Some((key, samples)));
    }
    let analysis = crate::vocal_analysis::analyze_clip(
        project,
        clip,
        sample_rate,
        root,
        &oc_dsp::vocal_analysis::DetectorParams::default(),
        None,
    )?;
    let reference_analysis = analyse_reference(project, tune, sample_rate, root)?;
    let resolved =
        resolve_tune_with_reference(project, clip, tune, &analysis, reference_analysis.as_ref())?;
    for warning in &resolved.warnings {
        crate::render_warnings::push(format!("@vocal_tune '{}': {warning}", tune.name));
    }
    for note in resolved.notes.iter().chain(&resolved.proposals) {
        for warning in &note.warnings {
            crate::render_warnings::push(format!(
                "@vocal_tune '{}': anchor {:.3}s: {warning}",
                tune.name, note.anchor_s
            ));
        }
    }
    let outcome = shift_window(
        sample_rate,
        &window,
        tune,
        &resolved,
        &analysis.analysis.notes,
    )?;
    if let Some(path) = &cached
        && let Some(parent) = path.parent()
    {
        let _ = fs::create_dir_all(parent);
        let _ = write_baked_wav(path, &outcome.samples, sample_rate);
    }
    Ok(Some((key, outcome.samples)))
}

/// Path of `to` relative to the directory `from`; both must be absolute.
fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for component in &to[common..] {
        out.push(component.as_os_str());
    }
    Some(out)
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn canonical_dir(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// Project state after baking: the block is gone and the clip reads the bake.
fn baked_project(
    project: &Project,
    clip_name: &str,
    tune_name: &str,
    file: &str,
) -> Result<Project, String> {
    let mut out = project.clone();
    out.properties
        .vocal_tunes
        .retain(|tune| tune.name != tune_name);
    let clip = out
        .audio_clips_mut()
        .find(|clip| clip.name == clip_name)
        .ok_or_else(|| format!("audio clip '{clip_name}' disappeared"))?;
    clip.file = file.to_string();
    clip.offset_seconds = 0.0;
    clip.stretch = 1.0;
    clip.stretch_mode = StretchMode::Off;
    clip.tempo_follow = false;
    Ok(out)
}

fn write_project(path: &Path, project: &Project) -> Result<(), String> {
    let source = oc_dsl::serialize(project);
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let write = || -> Result<(), String> {
        let mut file =
            File::create(&temp).map_err(|error| format!("{}: {error}", temp.display()))?;
        file.write_all(source.as_bytes())
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        fs::rename(&temp, path).map_err(|error| format!("{}: {error}", path.display()))
    };
    write().inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

/// Analyse a selector, resolve its stored decisions and bake the window.
///
/// `output_path` names the project to write; `None` only resolves and reports.
pub fn apply(
    project: &Project,
    project_path: &Path,
    selector: &str,
    sample_rate: u32,
    output_path: Option<&Path>,
) -> Result<AppliedTune, String> {
    let clip = crate::vocal_analysis::resolve_clip(project, selector)?;
    let tune = tune_for_clip(project, clip).ok_or_else(|| {
        format!(
            "audio clip '{}' has no @vocal_tune block; add one before applying",
            clip.name
        )
    })?;
    let analysis = crate::vocal_analysis::analyze_clip(
        project,
        clip,
        sample_rate,
        Some(parent_dir(project_path)),
        &oc_dsp::vocal_analysis::DetectorParams::default(),
        None,
    )?;
    let reference_analysis =
        analyse_reference(project, tune, sample_rate, Some(parent_dir(project_path)))?;
    let mut resolved =
        resolve_tune_with_reference(project, clip, tune, &analysis, reference_analysis.as_ref())?;
    let Some(output_path) = output_path else {
        return Ok(AppliedTune {
            resolved,
            baked: None,
            report: None,
            time: None,
            project_out: None,
        });
    };
    let input_dir = canonical_dir(parent_dir(project_path))?;
    let output_dir = canonical_dir(parent_dir(output_path))?;
    if input_dir != output_dir {
        return Err(format!(
            "the baked project must live next to the source project ({}); relative media paths would break in {}",
            input_dir.display(),
            output_dir.display()
        ));
    }
    let (key, window) = crate::vocal_analysis::clip_stereo_playback(
        project,
        clip,
        sample_rate,
        Some(parent_dir(project_path)),
    )?;
    if key.stamp() != resolved.analysis_key.stamp() {
        return Err(
            "the analysis key changed between analysis and bake; re-run the analysis".to_string(),
        );
    }
    let reference = reference_key(project, tune, sample_rate, Some(parent_dir(project_path)))?;
    let bake_key = BakeKey::new(key, tune, reference.as_ref().map(|(_, key)| key));
    let stem = output_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());
    let directory = output_dir
        .join(format!("{stem}.vocal"))
        .join(bake_key.dir_name());
    fs::create_dir_all(&directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    let path = directory.join(bake_key.file_name(&clip.name));
    let frames = window.len() / 2;
    let cached = read_baked(&path, frames, sample_rate);
    let from_cache = cached.is_some();
    let (samples, report, time) = match cached {
        Some(samples) => (samples, None, None),
        None => {
            let outcome = shift_window(
                sample_rate,
                &window,
                tune,
                &resolved,
                &analysis.analysis.notes,
            )?;
            write_baked_wav(&path, &outcome.samples, sample_rate)?;
            (outcome.samples, Some(outcome.report), outcome.time)
        }
    };
    measure_timing(&window, &samples, sample_rate, &mut resolved);
    let file = relative_path(&output_dir, &path).ok_or_else(|| {
        format!(
            "cannot express {} relative to {}",
            path.display(),
            output_dir.display()
        )
    })?;
    let file = file.to_string_lossy().replace('\\', "/");
    let baked = baked_project(project, &clip.name, &tune.name, &file)?;
    oc_dsl::validate(&baked)
        .map_err(|errors| format!("baked project failed validation: {errors:?}"))?;
    write_project(output_path, &baked)?;
    Ok(AppliedTune {
        resolved,
        baked: Some(BakedMedia {
            path,
            file,
            frames,
            sample_rate,
            from_cache,
        }),
        report,
        time,
        project_out: Some(output_path.to_path_buf()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_dsl::ast::{Accidental, BarBeat, KeyMode, KeyPoint};

    fn key(root: NoteName, accidental: Accidental, mode: KeyMode) -> KeySignature {
        KeySignature {
            root,
            accidental,
            mode,
        }
    }

    fn detected(start_s: f64, f0_hz: f64, confidence: f64) -> VocalNote {
        VocalNote {
            start_s,
            end_s: start_s + 0.4,
            onset_s: start_s,
            offset_s: start_s + 0.4,
            f0_median_hz: f0_hz,
            confidence,
            voiced_ratio: 1.0,
        }
    }

    fn note(at: f64, target: VocalTarget) -> VocalTuneNote {
        VocalTuneNote {
            at,
            target: Some(target),
            amount: None,
            retune_seconds: None,
            formant_cents: None,
            start_seconds: None,
            length: None,
            vibrato: None,
            bypass: false,
        }
    }

    fn project_with(properties: oc_dsl::ast::ProjectProperties) -> Project {
        Project {
            name: None,
            ocp_version: None,
            properties,
            presets: Vec::new(),
            tracks: Vec::new(),
            patterns: Vec::new(),
            buses: Vec::new(),
            master: None,
            sections: Vec::new(),
            markers: Vec::new(),
            groups: Vec::new(),
        }
    }

    fn test_clip() -> AudioClip {
        AudioClip {
            name: "lead".to_string(),
            track: "vocal".to_string(),
            file: "take.wav".to_string(),
            time_range: oc_dsl::ast::TimeRange {
                start: BarBeat {
                    bar: 1,
                    beat: 1,
                    subdivision: 0,
                },
                end: BarBeat {
                    bar: 2,
                    beat: 1,
                    subdivision: 0,
                },
            },
            placement: Default::default(),
            offset_seconds: 0.0,
            gain_db: 0.0,
            fade_in_seconds: 0.0,
            fade_out_seconds: 0.0,
            invert: false,
            crossfade_seconds: 0.0,
            stretch: 1.0,
            stretch_mode: StretchMode::Off,
            tempo_follow: false,
            pitch: 0.0,
            formant: 0.0,
            bypass_processing: false,
            preserve_formants: false,
            warp_markers: Vec::new(),
            loop_seconds: 0.0,
            take_group: None,
            take: None,
            muted: false,
            gain_envelope: Vec::new(),
        }
    }

    #[test]
    fn midi_cents_grid_is_documented_and_invertible() {
        assert!((hz_to_midi_cents(440.0) - 6_900.0).abs() < 1e-9);
        assert!((hz_to_midi_cents(220.0) - 5_700.0).abs() < 1e-9);
        assert!((midi_cents_to_hz(6_900.0) - 440.0).abs() < 1e-9);
        for cents in [0.0, 1_300.0, 5_700.0, 6_900.0, 12_700.0] {
            assert!((hz_to_midi_cents(midi_cents_to_hz(cents)) - cents).abs() < 1e-6);
        }
    }

    #[test]
    fn scale_degrees_follow_the_tonic_octave() {
        let a_major = key(NoteName::A, Accidental::Natural, KeyMode::Major);
        assert_eq!(scale_degree_midi(&a_major, 1, 4).unwrap(), 69);
        assert_eq!(scale_degree_midi(&a_major, 3, 4).unwrap(), 73);
        assert_eq!(scale_degree_midi(&a_major, 7, 3).unwrap(), 68);
        let c_major = key(NoteName::C, Accidental::Natural, KeyMode::Major);
        assert_eq!(scale_degree_midi(&c_major, 1, 4).unwrap(), 60);
        assert_eq!(scale_degree_midi(&c_major, 3, 4).unwrap(), 64);
        let a_minor = key(NoteName::A, Accidental::Natural, KeyMode::Minor);
        assert_eq!(scale_degree_midi(&a_minor, 3, 4).unwrap(), 72);
        let f_sharp = key(NoteName::F, Accidental::Sharp, KeyMode::Major);
        assert_eq!(scale_degree_midi(&f_sharp, 4, 4).unwrap(), 71);
        assert!(scale_degree_midi(&c_major, 8, 4).is_err());
        assert!(scale_degree_midi(&c_major, 1, -2).is_err());
    }

    #[test]
    fn absolute_targets_resolve_to_cents() {
        let detected_cents = hz_to_midi_cents(220.0);
        assert_eq!(
            resolve_target_cents(&VocalTarget::Pitch(69), detected_cents, None).unwrap(),
            6_900.0
        );
        assert_eq!(
            resolve_target_cents(&VocalTarget::Cents(5_712.0), detected_cents, None).unwrap(),
            5_712.0
        );
        assert_eq!(
            resolve_target_cents(&VocalTarget::Semitones(2.0), detected_cents, None).unwrap(),
            5_900.0
        );
        assert_eq!(
            resolve_target_cents(&VocalTarget::OffsetCents(-25.0), detected_cents, None).unwrap(),
            5_675.0
        );
        let c_major = key(NoteName::C, Accidental::Natural, KeyMode::Major);
        assert_eq!(
            resolve_target_cents(
                &VocalTarget::Degree {
                    degree: 3,
                    octave: 4
                },
                detected_cents,
                Some(&c_major)
            )
            .unwrap(),
            6_400.0
        );
        assert!(
            resolve_target_cents(
                &VocalTarget::Degree {
                    degree: 3,
                    octave: 4
                },
                detected_cents,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn amount_scales_the_shift() {
        let note = note(0.0, VocalTarget::Pitch(69));
        let detected = detected(0.0, 220.0, 0.9);
        let resolved = resolve_override(&note, &detected, None, 100.0, CONFIDENCE_FLOOR).unwrap();
        assert_eq!(resolved.outcome, NoteOutcome::Shift);
        assert!((resolved.shift_cents.unwrap() - 1_200.0).abs() < 1e-9);
        assert!((resolved.resulting_hz.unwrap() - 440.0).abs() < 1e-6);

        let mut half = note.clone();
        half.amount = Some(50.0);
        let resolved = resolve_override(&half, &detected, None, 100.0, CONFIDENCE_FLOOR).unwrap();
        assert!((resolved.shift_cents.unwrap() - 600.0).abs() < 1e-9);
        assert!((resolved.resulting_hz.unwrap() - 311.126_983_6).abs() < 1e-3);

        let resolved = resolve_override(&note, &detected, None, 0.0, CONFIDENCE_FLOOR).unwrap();
        assert_eq!(resolved.outcome, NoteOutcome::PassThrough);
        assert_eq!(resolved.shift_cents, Some(0.0));
    }

    #[test]
    fn targets_above_or_below_an_octave_still_resolve_and_warn() {
        let detected = detected(0.0, 110.0, 0.9);
        let resolved = resolve_override(
            &note(0.0, VocalTarget::Pitch(69)),
            &detected,
            None,
            100.0,
            CONFIDENCE_FLOOR,
        )
        .unwrap();
        assert!((resolved.shift_cents.unwrap() - 2_400.0).abs() < 1e-9);
        // One warning for the octave jump, one for the clamp the shifter applies.
        assert_eq!(resolved.warnings.len(), 2);
        assert!(resolved.warnings[0].contains("more than an octave"));
        assert!(resolved.warnings[1].contains("clamped"));

        let resolved = resolve_override(
            &note(0.0, VocalTarget::Pitch(21)),
            &detected,
            None,
            100.0,
            CONFIDENCE_FLOOR,
        )
        .unwrap();
        assert!((resolved.shift_cents.unwrap() + 2_400.0).abs() < 1e-9);
        assert_eq!(resolved.warnings.len(), 2);
    }

    #[test]
    fn low_confidence_is_left_untouched_and_reported() {
        let detected = detected(0.0, 220.0, 0.3);
        let resolved = resolve_override(
            &note(0.0, VocalTarget::Pitch(69)),
            &detected,
            None,
            100.0,
            CONFIDENCE_FLOOR,
        )
        .unwrap();
        assert_eq!(resolved.outcome, NoteOutcome::LowConfidence);
        assert_eq!(resolved.shift_cents, None);
        assert_eq!(resolved.resulting_hz, None);
        assert!(resolved.warnings[0].contains("below the 0.50 floor"));
    }

    #[test]
    fn bypass_wins_over_a_target() {
        let mut bypassed = note(0.0, VocalTarget::Pitch(69));
        bypassed.bypass = true;
        let resolved = resolve_override(
            &bypassed,
            &detected(0.0, 220.0, 0.9),
            None,
            100.0,
            CONFIDENCE_FLOOR,
        )
        .unwrap();
        assert_eq!(resolved.outcome, NoteOutcome::Bypassed);
        assert_eq!(resolved.shift_cents, None);
    }

    #[test]
    fn anchor_matching_uses_the_tolerance_and_reports_stale_ones() {
        let notes = [detected(1.0, 220.0, 0.9), detected(2.0, 330.0, 0.9)];
        assert_eq!(match_note(&notes, 1.010), Some(0));
        assert_eq!(match_note(&notes, 1.029), Some(0));
        assert_eq!(match_note(&notes, 1.031), None);
        assert_eq!(match_note(&notes, 1.9), None);
        assert_eq!(match_note(&notes, 2.0), Some(1));
    }

    #[test]
    fn key_at_uses_key_map_points_then_the_project_key() {
        let mut properties = oc_dsl::ast::ProjectProperties {
            tempo: 120.0,
            key: Some(key(NoteName::C, Accidental::Natural, KeyMode::Major)),
            key_map: vec![
                KeyPoint {
                    position: BarBeat::new(3, 1, 0),
                    key: key(NoteName::D, Accidental::Natural, KeyMode::Major),
                },
                KeyPoint {
                    position: BarBeat::new(5, 1, 0),
                    key: key(NoteName::E, Accidental::Natural, KeyMode::Minor),
                },
            ],
            ..Default::default()
        };
        properties.key_map.sort_by_key(|point| point.position.bar);
        let project = project_with(properties);
        let clock = ProjectClock::new(&project.properties).unwrap();
        // 120 bpm: one bar is 2 s.
        assert_eq!(
            key_at(&project, &clock, 1.0).unwrap().unwrap().root,
            NoteName::C
        );
        assert_eq!(
            key_at(&project, &clock, 4.5).unwrap().unwrap().root,
            NoteName::D
        );
        assert_eq!(
            key_at(&project, &clock, 9.0).unwrap().unwrap().mode,
            KeyMode::Minor
        );

        let no_project_key = project_with(oc_dsl::ast::ProjectProperties {
            tempo: 120.0,
            ..Default::default()
        });
        let clock = ProjectClock::new(&no_project_key.properties).unwrap();
        assert!(key_at(&no_project_key, &clock, 1.0).unwrap().is_none());
    }

    #[test]
    fn bake_key_changes_with_every_decision_and_version() {
        let analysis = VocalAnalysisKey::new(
            crate::derived_cache::StretchKey::new(
                crate::media_cache::MediaKey {
                    path: PathBuf::from("/tmp/take.wav"),
                    mtime: (1_700_000_000, 7),
                    size: 4_096,
                },
                48_000,
                1.0,
                StretchMode::Off,
                crate::stretch::ALGO_VERSION,
            ),
            oc_dsp::vocal_analysis::DetectorParams::default(),
        );
        let mut tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            ..VocalTune::default()
        };
        tune.notes.push(note(1.0, VocalTarget::Pitch(69)));
        let base = BakeKey::new(analysis.clone(), &tune, None);
        assert!(base.file_name("lead").starts_with("lead-"));
        assert_eq!(base.dir_name(), format!("v{}", version_stamp()));
        assert!(
            base.stamp()
                .contains(&format!("|{}.{}", BAKE_VERSION, vocal_shift::ALGO_VERSION))
        );

        let same = BakeKey::new(analysis.clone(), &tune, None);
        assert_eq!(base.file_name("lead"), same.file_name("lead"));

        let mut moved = tune.clone();
        moved.notes[0].at = 2.0;
        assert_ne!(
            base.file_name("lead"),
            BakeKey::new(analysis.clone(), &moved, None).file_name("lead")
        );

        let mut retargeted = tune.clone();
        retargeted.notes[0].target = Some(VocalTarget::Pitch(70));
        assert_ne!(
            base.file_name("lead"),
            BakeKey::new(analysis.clone(), &retargeted, None).file_name("lead")
        );

        let mut half = tune.clone();
        half.notes[0].amount = Some(50.0);
        assert_ne!(
            base.file_name("lead"),
            BakeKey::new(analysis.clone(), &half, None).file_name("lead")
        );

        let mut flattened = tune.clone();
        flattened.notes[0].vibrato = Some(VocalVibratoMode::Flatten);
        assert_ne!(
            base.file_name("lead"),
            BakeKey::new(analysis.clone(), &flattened, None).file_name("lead")
        );

        let mut analyzer = analysis.clone();
        analyzer.detector.split_cents += 1.0;
        assert_ne!(
            base.file_name("lead"),
            BakeKey::new(analyzer, &tune, None).file_name("lead")
        );

        let mut versioned = base.clone();
        versioned.version = BAKE_VERSION + 1;
        assert_ne!(base.file_name("lead"), versioned.file_name("lead"));
        assert_ne!(base.dir_name(), versioned.dir_name());

        // A shifter change alone must invalidate: same decisions, same
        // analysis, different algorithm version.
        let shifted = BakeKey::with_versions(
            analysis.clone(),
            &tune,
            None,
            BAKE_VERSION,
            vocal_shift::ALGO_VERSION + 1,
        );
        assert_ne!(base.file_name("lead"), shifted.file_name("lead"));
        assert_ne!(base.dir_name(), shifted.dir_name());
        assert!(shifted.stamp().contains(&format!(
            "|{}.{}",
            BAKE_VERSION,
            vocal_shift::ALGO_VERSION + 1
        )));
    }

    #[test]
    fn the_bake_key_covers_the_timing_fields() {
        let analysis = VocalAnalysisKey::new(
            crate::derived_cache::StretchKey::new(
                crate::media_cache::MediaKey {
                    path: PathBuf::from("/tmp/take.wav"),
                    mtime: (1_700_000_000, 7),
                    size: 4_096,
                },
                48_000,
                1.0,
                StretchMode::Off,
                crate::stretch::ALGO_VERSION,
            ),
            oc_dsp::vocal_analysis::DetectorParams::default(),
        );
        let mut tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            ..VocalTune::default()
        };
        let mut timed = note(1.0, VocalTarget::Pitch(69));
        timed.start_seconds = Some(0.030);
        timed.length = Some(1.08);
        tune.notes.push(timed);
        let base = BakeKey::new(analysis.clone(), &tune, None);
        assert_eq!(
            base.dir_name(),
            format!("v{BAKE_VERSION}.{}", vocal_shift::ALGO_VERSION)
        );
        let stamp = decision_stamp(&tune);
        assert!(stamp.contains("start="), "{stamp}");
        assert!(stamp.contains("length="), "{stamp}");
        assert!(stamp.contains("max_move="), "{stamp}");

        let mut moved = tune.clone();
        moved.notes[0].start_seconds = Some(-0.030);
        assert_ne!(
            base.file_name("lead"),
            BakeKey::new(analysis.clone(), &moved, None).file_name("lead")
        );

        let mut stretched = tune.clone();
        stretched.notes[0].length = Some(1.2);
        assert_ne!(
            base.file_name("lead"),
            BakeKey::new(analysis.clone(), &stretched, None).file_name("lead")
        );

        let mut capped = tune.clone();
        capped.max_move_seconds = 0.2;
        assert_ne!(
            base.file_name("lead"),
            BakeKey::new(analysis.clone(), &capped, None).file_name("lead")
        );
    }

    #[test]
    fn a_zero_move_resolves_to_no_edit_and_a_pass_through() {
        let mut tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            snap: VocalSnapPolicy::Off,
            ..VocalTune::default()
        };
        let mut timed = VocalTuneNote {
            start_seconds: Some(0.0),
            length: Some(1.0),
            ..note(0.2, VocalTarget::OffsetCents(0.0))
        };
        timed.target = None;
        tune.notes.push(timed);
        let analysis = analysis_of(vec![detected(0.2, 220.0, 0.9)], 0.0);
        let project = project_with(oc_dsl::ast::ProjectProperties::default());
        let resolved = resolve_tune(&project, &test_clip(), &tune, &analysis).unwrap();
        assert!(resolved.timing.is_empty(), "a no-op edit survived");
        assert!(resolved.pass_through());
    }

    #[test]
    fn a_length_change_beyond_the_cap_is_rejected() {
        let mut tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            snap: VocalSnapPolicy::Off,
            ..VocalTune::default()
        };
        tune.notes.push(VocalTuneNote {
            length: Some(2.0),
            ..note(0.2, VocalTarget::OffsetCents(25.0))
        });
        // The detected note is 0.4 s, so 2.0 doubles it by 400 ms: over the cap.
        let analysis = analysis_of(vec![detected(0.2, 220.0, 0.9)], 0.0);
        let project = project_with(oc_dsl::ast::ProjectProperties::default());
        let error = resolve_tune(&project, &test_clip(), &tune, &analysis).unwrap_err();
        assert!(error.contains("max_move"), "{error}");
        assert!(error.contains("length"), "{error}");
    }

    #[test]
    fn a_timing_edit_moves_its_pitch_segment_to_the_destination() {
        let mut tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            snap: VocalSnapPolicy::Off,
            ..VocalTune::default()
        };
        let mut timed = note(0.2, VocalTarget::OffsetCents(25.0));
        timed.start_seconds = Some(0.030);
        tune.notes.push(timed);
        let analysis = analysis_of(vec![detected(0.2, 220.0, 0.9)], 0.0);
        let project = project_with(oc_dsl::ast::ProjectProperties::default());
        let resolved = resolve_tune(&project, &test_clip(), &tune, &analysis).unwrap();
        assert_eq!(resolved.timing.len(), 1);
        let frames = 48_000 * 4;
        let curve =
            shift_curve(frames, 48_000, &tune, &resolved, &analysis.analysis.notes).unwrap();
        assert_eq!(curve.segments().len(), 1);
        let segment = curve.segments()[0];
        assert_eq!(segment.start, ((0.2 + 0.030) * 48_000.0) as usize);
        assert_eq!(segment.end, ((0.2 + 0.030 + 0.4) * 48_000.0) as usize);
        assert!((segment.cents - 25.0).abs() < 1e-3);
    }

    #[test]
    fn a_shift_curve_covers_exactly_the_shifting_note_spans() {
        let mut tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            ..VocalTune::default()
        };
        // 0.2 s untouched (no override), 1.2 s low confidence, 2.2 s bypass,
        // 3.2 s a real shift.
        let analysis = ClipAnalysis {
            clip_name: "lead".to_string(),
            track: "vocal".to_string(),
            file: "take.wav".to_string(),
            target_rate: 48_000,
            clip_start_s: 0.0,
            duration_s: 4.0,
            analysis: oc_dsp::vocal_analysis::VocalAnalysis {
                analysis_rate_hz: 16_000.0,
                hop_seconds: 0.005,
                duration_s: 4.0,
                frames: Vec::new(),
                notes: vec![
                    detected(0.2, 220.0, 0.9),
                    detected(1.2, 330.0, 0.3),
                    detected(2.2, 440.0, 0.9),
                    detected(3.2, 500.0, 0.9),
                ],
            },
            from_cache: false,
            output: None,
            key: VocalAnalysisKey::new(
                crate::derived_cache::StretchKey::new(
                    crate::media_cache::MediaKey {
                        path: PathBuf::from("/tmp/take.wav"),
                        mtime: (1_700_000_000, 7),
                        size: 4_096,
                    },
                    48_000,
                    1.0,
                    StretchMode::Off,
                    crate::stretch::ALGO_VERSION,
                ),
                oc_dsp::vocal_analysis::DetectorParams::default(),
            ),
        };
        let project = project_with(oc_dsl::ast::ProjectProperties::default());
        tune.notes.push(note(1.2, VocalTarget::OffsetCents(25.0)));
        tune.notes.push(VocalTuneNote {
            bypass: true,
            ..note(2.2, VocalTarget::OffsetCents(25.0))
        });
        tune.notes.push(note(3.2, VocalTarget::OffsetCents(25.0)));
        let resolved = resolve_tune(&project, &test_clip(), &tune, &analysis).unwrap();
        assert_eq!(resolved.shifted_notes(), 1);
        assert_eq!(resolved.notes[0].outcome, NoteOutcome::LowConfidence);
        assert_eq!(resolved.notes[1].outcome, NoteOutcome::Bypassed);

        let frames = 48_000 * 4;
        let curve =
            shift_curve(frames, 48_000, &tune, &resolved, &analysis.analysis.notes).unwrap();
        assert_eq!(curve.segments().len(), 1);
        let segment = curve.segments()[0];
        assert_eq!(segment.start, (3.2 * 48_000.0) as usize);
        assert_eq!(segment.end, ((3.2 + 0.4) * 48_000.0) as usize);
        assert!((segment.cents - 25.0).abs() < 1e-3);
        // Every other note's span is outside the curve, so it cannot move.
        for second in [0.4, 1.4, 2.4] {
            assert_eq!(curve.cents_at((second * 48_000.0) as usize), 0.0);
        }
    }

    fn analysis_of(notes: Vec<VocalNote>, clip_start_s: f64) -> ClipAnalysis {
        ClipAnalysis {
            clip_name: "lead".to_string(),
            track: "vocal".to_string(),
            file: "take.wav".to_string(),
            target_rate: 48_000,
            clip_start_s,
            duration_s: 4.0,
            analysis: oc_dsp::vocal_analysis::VocalAnalysis {
                analysis_rate_hz: 16_000.0,
                hop_seconds: 0.005,
                duration_s: 4.0,
                frames: Vec::new(),
                notes,
            },
            from_cache: false,
            output: None,
            key: VocalAnalysisKey::new(
                crate::derived_cache::StretchKey::new(
                    crate::media_cache::MediaKey {
                        path: PathBuf::from("/tmp/take.wav"),
                        mtime: (1_700_000_000, 7),
                        size: 4_096,
                    },
                    48_000,
                    1.0,
                    StretchMode::Off,
                    crate::stretch::ALGO_VERSION,
                ),
                oc_dsp::vocal_analysis::DetectorParams::default(),
            ),
        }
    }

    fn project_with_key(key: KeySignature) -> Project {
        project_with(oc_dsl::ast::ProjectProperties {
            key: Some(key),
            ..Default::default()
        })
    }

    #[test]
    fn scale_snap_fills_notes_without_a_target() {
        let project = project_with_key(key(NoteName::C, Accidental::Natural, KeyMode::Major));
        let tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            ..VocalTune::default()
        };
        // D4 + 4 cents is untouched; E4 - 20 cents snaps up to E4.
        let analysis = analysis_of(
            vec![
                detected(0.2, midi_cents_to_hz(6_204.0), 0.9),
                detected(1.2, midi_cents_to_hz(6_380.0), 0.9),
            ],
            0.0,
        );
        let resolved = resolve_tune(&project, &test_clip(), &tune, &analysis).unwrap();
        assert_eq!(resolved.snap, VocalSnapPolicy::Scale);
        assert!(resolved.notes.is_empty());
        assert_eq!(resolved.proposals.len(), 2);
        assert_eq!(resolved.proposals[0].outcome, NoteOutcome::InTune);
        assert_eq!(resolved.proposals[0].target_cents, Some(6_200.0));
        assert_eq!(resolved.proposals[0].shift_cents, Some(0.0));
        assert_eq!(resolved.proposals[1].outcome, NoteOutcome::Snapped);
        assert_eq!(resolved.proposals[1].target_cents, Some(6_400.0));
        assert!((resolved.proposals[1].shift_cents.unwrap() - 20.0).abs() < 1e-9);
        assert!(
            (resolved.proposals[1].resulting_hz.unwrap() - midi_cents_to_hz(6_400.0)).abs() < 1e-9
        );
        assert_eq!(resolved.unoverridden_notes, 2);
        assert_eq!(resolved.shifted_notes(), 1);
        let frames = 48_000 * 4;
        let curve =
            shift_curve(frames, 48_000, &tune, &resolved, &analysis.analysis.notes).unwrap();
        assert_eq!(curve.segments().len(), 1);
        assert_eq!(curve.segments()[0].start, (1.2 * 48_000.0) as usize);
    }

    #[test]
    fn a_stored_target_beats_the_reference_and_the_scale() {
        let c_major = key(NoteName::C, Accidental::Natural, KeyMode::Major);
        let project = project_with_key(c_major);
        let mut tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            scale: Some(c_major),
            reference: Some("guide".to_string()),
            snap: VocalSnapPolicy::Reference,
            ..VocalTune::default()
        };
        tune.notes.push(note(0.2, VocalTarget::Pitch(69)));
        let analysis = analysis_of(
            vec![detected(0.2, 220.0, 0.9), detected(1.2, 215.0, 0.9)],
            0.0,
        );
        let reference = analysis_of(
            vec![detected(0.2, 220.0, 0.9), detected(1.2, 440.0, 0.9)],
            0.0,
        );
        let resolved =
            resolve_tune_with_reference(&project, &test_clip(), &tune, &analysis, Some(&reference))
                .unwrap();
        assert_eq!(resolved.snap, VocalSnapPolicy::Reference);
        assert_eq!(resolved.notes.len(), 1);
        assert_eq!(resolved.notes[0].outcome, NoteOutcome::Shift);
        assert_eq!(resolved.notes[0].target_cents, Some(6_900.0));
        assert_eq!(resolved.proposals.len(), 1);
        // The reference is an octave up; folding keeps the take in its own
        // register instead of dragging it to A4.
        assert_eq!(resolved.proposals[0].target_cents, Some(5_700.0));
        assert_eq!(resolved.reference_notes, 1);
        assert!(
            resolved.proposals[0].shift_cents.unwrap() < 100.0,
            "{:?}",
            resolved.proposals[0].shift_cents
        );
    }

    #[test]
    fn bypass_beats_the_automatic_pass() {
        let project = project_with_key(key(NoteName::C, Accidental::Natural, KeyMode::Major));
        let mut tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            ..VocalTune::default()
        };
        let mut bypassed = note(0.2, VocalTarget::Pitch(69));
        bypassed.bypass = true;
        tune.notes.push(bypassed);
        let analysis = analysis_of(
            vec![
                detected(0.2, midi_cents_to_hz(6_380.0), 0.9),
                detected(1.2, midi_cents_to_hz(6_380.0), 0.9),
            ],
            0.0,
        );
        let resolved = resolve_tune(&project, &test_clip(), &tune, &analysis).unwrap();
        assert_eq!(resolved.notes[0].outcome, NoteOutcome::Bypassed);
        assert_eq!(resolved.notes[0].shift_cents, None);
        assert_eq!(resolved.proposals.len(), 1);
        assert_eq!(resolved.proposals[0].detected_index, Some(1));
        assert_eq!(resolved.proposals[0].outcome, NoteOutcome::Snapped);
    }

    #[test]
    fn a_low_confidence_note_is_left_alone_and_counted() {
        let project = project_with_key(key(NoteName::C, Accidental::Natural, KeyMode::Major));
        let tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            ..VocalTune::default()
        };
        let analysis = analysis_of(vec![detected(0.2, midi_cents_to_hz(6_380.0), 0.3)], 0.0);
        let resolved = resolve_tune(&project, &test_clip(), &tune, &analysis).unwrap();
        assert_eq!(resolved.proposals[0].outcome, NoteOutcome::LowConfidence);
        assert_eq!(resolved.proposals[0].target_cents, None);
        assert_eq!(resolved.shifted_notes(), 0);
        assert!(
            resolved
                .warnings
                .iter()
                .any(|warning| warning.contains("1 low-confidence note(s)")),
            "{:?}",
            resolved.warnings
        );
    }

    #[test]
    fn a_reference_gap_falls_back_to_the_scale() {
        let c_major = key(NoteName::C, Accidental::Natural, KeyMode::Major);
        let project = project_with_key(c_major);
        let mut tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            reference: Some("guide".to_string()),
            snap: VocalSnapPolicy::Reference,
            ..VocalTune::default()
        };
        tune.notes.clear();
        let analysis = analysis_of(
            vec![
                detected(0.2, midi_cents_to_hz(6_380.0), 0.9),
                detected(1.2, midi_cents_to_hz(6_380.0), 0.9),
            ],
            0.0,
        );
        // The reference only has a note under the first take note, 80 cents up.
        let reference = analysis_of(vec![detected(0.2, midi_cents_to_hz(6_460.0), 0.9)], 0.0);
        let resolved =
            resolve_tune_with_reference(&project, &test_clip(), &tune, &analysis, Some(&reference))
                .unwrap();
        assert_eq!(resolved.reference_notes, 1);
        assert_eq!(resolved.reference_fallbacks, 1);
        assert_eq!(resolved.proposals[0].target_cents, Some(6_460.0));
        assert_eq!(resolved.proposals[1].target_cents, Some(6_400.0));
        assert!(
            resolved
                .warnings
                .iter()
                .any(|warning| warning.contains("had no reference note")),
            "{:?}",
            resolved.warnings
        );
    }

    #[test]
    fn a_reference_that_does_not_overlap_warns_and_uses_the_scale() {
        let c_major = key(NoteName::C, Accidental::Natural, KeyMode::Major);
        let project = project_with_key(c_major);
        let tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            reference: Some("guide".to_string()),
            snap: VocalSnapPolicy::Reference,
            ..VocalTune::default()
        };
        let analysis = analysis_of(vec![detected(0.2, midi_cents_to_hz(6_380.0), 0.9)], 0.0);
        // The reference sits 10 s later on the timeline.
        let reference = analysis_of(vec![detected(0.2, midi_cents_to_hz(6_390.0), 0.9)], 10.0);
        let resolved =
            resolve_tune_with_reference(&project, &test_clip(), &tune, &analysis, Some(&reference))
                .unwrap();
        assert!(
            resolved
                .warnings
                .iter()
                .any(|warning| warning.contains("does not overlap the take")),
            "{:?}",
            resolved.warnings
        );
        // The scale still moved the note, so the block is not a silent no-op.
        assert_eq!(resolved.proposals[0].outcome, NoteOutcome::Snapped);
        assert_eq!(resolved.proposals[0].target_cents, Some(6_400.0));
    }

    #[test]
    fn snap_off_keeps_the_stored_decisions_only() {
        let project = project_with_key(key(NoteName::C, Accidental::Natural, KeyMode::Major));
        let mut tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            snap: VocalSnapPolicy::Off,
            ..VocalTune::default()
        };
        tune.notes.push(note(0.2, VocalTarget::Pitch(69)));
        let analysis = analysis_of(
            vec![
                detected(0.2, midi_cents_to_hz(6_380.0), 0.9),
                detected(1.2, midi_cents_to_hz(6_380.0), 0.9),
            ],
            0.0,
        );
        let resolved = resolve_tune(&project, &test_clip(), &tune, &analysis).unwrap();
        assert_eq!(resolved.snap, VocalSnapPolicy::Off);
        assert!(resolved.proposals.is_empty());
        assert_eq!(resolved.notes.len(), 1);
        assert_eq!(resolved.notes[0].outcome, NoteOutcome::Shift);
        assert_eq!(resolved.unoverridden_notes, 1);
    }

    #[test]
    fn an_amount_of_zero_disables_the_automatic_pass() {
        let project = project_with_key(key(NoteName::C, Accidental::Natural, KeyMode::Major));
        let tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            amount: 0.0,
            ..VocalTune::default()
        };
        let analysis = analysis_of(vec![detected(0.2, midi_cents_to_hz(6_380.0), 0.9)], 0.0);
        let resolved = resolve_tune(&project, &test_clip(), &tune, &analysis).unwrap();
        assert!(resolved.proposals.is_empty());
        assert!(resolved.pass_through());
    }

    #[test]
    fn scale_snap_follows_the_key_map_at_the_note_position() {
        let c_major = key(NoteName::C, Accidental::Natural, KeyMode::Major);
        let a_major = key(NoteName::A, Accidental::Natural, KeyMode::Major);
        let properties = oc_dsl::ast::ProjectProperties {
            tempo: 120.0,
            key: Some(c_major),
            key_map: vec![KeyPoint {
                position: BarBeat::new(2, 1, 0),
                key: a_major,
            }],
            ..Default::default()
        };
        let project = project_with(properties);
        let tune = VocalTune {
            name: "lead_tune".to_string(),
            clip: Some("lead".to_string()),
            ..VocalTune::default()
        };
        // C#4 + 10 cents: in C major it belongs to D4 (90 cents up, the only
        // scale pitch inside the cap), in A major it is already C#4.
        let analysis = analysis_of(
            vec![
                detected(1.0, midi_cents_to_hz(6_110.0), 0.9),
                detected(3.0, midi_cents_to_hz(6_110.0), 0.9),
            ],
            0.0,
        );
        let resolved = resolve_tune(&project, &test_clip(), &tune, &analysis).unwrap();
        assert_eq!(resolved.proposals[0].target_cents, Some(6_200.0));
        assert_eq!(resolved.proposals[1].target_cents, Some(6_100.0));
    }

    #[test]
    fn decision_stamp_covers_the_snap_fields() {
        let tune = VocalTune {
            name: "t".to_string(),
            clip: Some("lead".to_string()),
            ..VocalTune::default()
        };
        let base = decision_stamp(&tune);
        let mut reference = tune.clone();
        reference.reference = Some("guide".to_string());
        assert_ne!(base, decision_stamp(&reference));
        let mut policy = tune.clone();
        policy.snap = VocalSnapPolicy::Reference;
        assert_ne!(base, decision_stamp(&policy));
        let mut capped = tune.clone();
        capped.snap_cap_cents = 50.0;
        assert_ne!(base, decision_stamp(&capped));
        let mut windowed = tune.clone();
        windowed.snap_window_cents = 20.0;
        assert_ne!(base, decision_stamp(&windowed));
    }

    #[test]
    fn the_bake_key_includes_the_reference_analysis() {
        let analysis = analysis_of(Vec::new(), 0.0).key;
        let tune = VocalTune {
            name: "t".to_string(),
            clip: Some("lead".to_string()),
            reference: Some("guide".to_string()),
            snap: VocalSnapPolicy::Reference,
            ..VocalTune::default()
        };
        let mut reference = analysis_of(Vec::new(), 0.0).key;
        let base = BakeKey::new(analysis.clone(), &tune, Some(&reference));
        let same = BakeKey::new(analysis.clone(), &tune, Some(&reference.clone()));
        assert_eq!(base.file_name("lead"), same.file_name("lead"));
        reference.offset_frames += 1;
        let moved = BakeKey::new(analysis.clone(), &tune, Some(&reference));
        assert_ne!(base.file_name("lead"), moved.file_name("lead"));
        let absent = BakeKey::new(analysis, &tune, None);
        assert_ne!(base.file_name("lead"), absent.file_name("lead"));
    }

    #[test]
    fn relative_paths_stay_inside_the_project_directory() {
        let dir = Path::new("/tmp/work/project");
        assert_eq!(
            relative_path(dir, &dir.join("song.vocal/v1/lead-1234.wav")).unwrap(),
            PathBuf::from("song.vocal/v1/lead-1234.wav")
        );
        assert_eq!(
            relative_path(dir, Path::new("/tmp/other/x.wav")).unwrap(),
            PathBuf::from("../../other/x.wav")
        );
    }
}
