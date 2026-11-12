//! Harmony voices for `@vocal_harmony`.
//!
//! One harmony is a second part on the clip's own track: the clip's tuned
//! playback window shifted by a diatonic interval inside the clip's scale,
//! through the same offline shifter and per-note curve the tune pass uses. This
//! module resolves the interval, builds the curve, bakes and caches the voice
//! and mixes it into the track buffer. Offline/worker path only; the audio
//! callback never calls this. See `docs/vocal-tune.md`.

use std::fs;
use std::path::{Path, PathBuf};

use oc_dsl::ast::{AudioClip, KeySignature, Project, VocalHarmony, VocalTune};
use oc_dsl::timing::ProjectClock;
use oc_dsp::vocal_analysis::VocalNote;
use oc_dsp::vocal_shift::{self, ShiftConfig, ShiftCurve, ShiftSegment};

use crate::vocal_analysis::{ClipAnalysis, VocalAnalysisKey};
use crate::vocal_snap;

/// Bump when any change here can move a baked harmony sample.
///
/// The lead's [`crate::vocal_tune::BAKE_VERSION`] stays where it is: a harmony
/// edit must not invalidate the lead's bake, and the tune bake did not change.
pub const HARMONY_VERSION: u32 = 1;
/// A harmony this far from the lead note is refused: the shifter stops there.
pub const MAX_INTERVAL_CENTS: f64 = vocal_shift::MAX_SHIFT_CENTS as f64;
/// Glide into a note's shift when the clip has no `@vocal_tune` to inherit.
pub const DEFAULT_RETUNE_SECONDS: f64 = 0.025;

/// Harmony algorithm identity as reported: `<harmony>.<shifter>`.
pub fn version_stamp() -> String {
    format!("{HARMONY_VERSION}.{}", vocal_shift::ALGO_VERSION)
}

/// The harmony block bound to a clip, if any.
pub fn harmony_for_clip<'a>(project: &'a Project, clip: &AudioClip) -> Option<&'a VocalHarmony> {
    project.properties.vocal_harmonies.iter().find(|harmony| {
        harmony.clip.as_deref() == Some(clip.name.as_str())
            || (harmony.clip.is_none() && harmony.track.as_deref() == Some(clip.track.as_str()))
    })
}

/// The `@vocal_tune` block on the same clip: it supplies the key and the glide.
fn tune_for_clip<'a>(project: &'a Project, clip: &AudioClip) -> Option<&'a VocalTune> {
    crate::vocal_tune::tune_for_clip(project, clip)
}

/// One resolved harmony note: the lead's detected pitch and the voice's target.
#[derive(Debug, Clone, PartialEq)]
pub struct HarmonyNote {
    pub detected_index: usize,
    pub start_s: f64,
    pub end_s: f64,
    /// Detected lead pitch in MIDI cents.
    pub lead_cents: f64,
    /// Voice pitch in MIDI cents.
    pub target_cents: f64,
    /// `target_cents - lead_cents`.
    pub shift_cents: f64,
}

/// What the resolver decided for a whole harmony voice.
#[derive(Debug, Clone, PartialEq)]
pub struct HarmonyPlan {
    pub harmony: String,
    pub clip: String,
    pub interval: i32,
    pub level_db: f64,
    pub pan: f64,
    /// No key was active, so `interval` counted chromatic semitones.
    pub chromatic: bool,
    pub notes: Vec<HarmonyNote>,
    pub warnings: Vec<String>,
}

impl HarmonyPlan {
    /// Frames of glide for the voice: the clip's tune `retune`, else the default.
    fn retune_seconds(&self, tune: Option<&VocalTune>) -> f64 {
        tune.map_or(DEFAULT_RETUNE_SECONDS, |tune| tune.retune_seconds)
    }
}

/// Pitch name of a cents value, nearest equal-tempered note.
fn pitch_name(cents: f64) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let midi = (cents / 100.0).round() as i32;
    let octave = midi.div_euclid(12) - 1;
    format!("{}{}", NAMES[midi.rem_euclid(12) as usize], octave)
}

/// Every scale pitch inside the MIDI range, ascending.
fn scale_grid(scale: &KeySignature) -> Vec<u8> {
    let mask = vocal_snap::scale_pitch_classes(scale);
    (0..=127_u8)
        .filter(|midi| mask[usize::from(*midi) % 12])
        .collect()
}

/// The voice pitch `interval` scale steps from `lead_cents`.
///
/// The lead pitch snaps to the nearest scale pitch first, so the voice is a
/// degree of the key rather than the lead's own deviation plus a step.
fn diatonic_target(scale: &KeySignature, lead_cents: f64, interval: i32) -> Option<f64> {
    let grid = scale_grid(scale);
    let mask = vocal_snap::scale_pitch_classes(scale);
    let nearest = vocal_snap::nearest_scale_cents(lead_cents, &mask)?;
    let midi = (nearest / 100.0).round() as i32;
    let index = grid.iter().position(|note| i32::from(*note) == midi)?;
    let target = index as i32 + interval;
    if target < 0 || target >= grid.len() as i32 {
        return None;
    }
    Some(f64::from(grid[target as usize]) * 100.0)
}

/// Resolve every detected note to its harmony pitch.
///
/// The key comes from the clip's `@vocal_tune` scale first, then `key_map` at
/// the note's position, then `@project key`. Without any key the interval
/// counts chromatic semitones and the plan reports it.
pub fn resolve_harmony(
    project: &Project,
    clip: &AudioClip,
    harmony: &VocalHarmony,
    analysis: &ClipAnalysis,
) -> Result<HarmonyPlan, String> {
    let tune = tune_for_clip(project, clip);
    let clock = ProjectClock::new(&project.properties)?;
    let mut plan = HarmonyPlan {
        harmony: harmony.name.clone(),
        clip: clip.name.clone(),
        interval: harmony.interval,
        level_db: harmony.level_db,
        pan: harmony.pan,
        chromatic: false,
        notes: Vec::new(),
        warnings: Vec::new(),
    };
    if analysis.analysis.notes.is_empty() {
        plan.warnings
            .push("the clip has no detected notes, so the harmony voice is silent".to_string());
        return Ok(plan);
    }
    let mut voice = |note: &VocalNote, index: usize| -> Result<(), String> {
        let lead_cents = crate::vocal_tune::hz_to_midi_cents(note.f0_median_hz);
        let scale = match tune.and_then(|tune| tune.scale) {
            Some(scale) => Some(scale),
            None => {
                crate::vocal_tune::key_at(project, &clock, analysis.clip_start_s + note.start_s)?
            }
        };
        let target_cents = match scale {
            Some(scale) => {
                diatonic_target(&scale, lead_cents, harmony.interval).ok_or_else(|| {
                    format!(
                        "@vocal_harmony '{}': interval {} on {} at {:.3}s leaves the MIDI range",
                        harmony.name,
                        harmony.interval,
                        pitch_name(lead_cents),
                        note.start_s
                    )
                })?
            }
            None => {
                plan.chromatic = true;
                lead_cents + f64::from(harmony.interval) * 100.0
            }
        };
        let shift_cents = target_cents - lead_cents;
        if shift_cents.abs() > MAX_INTERVAL_CENTS {
            return Err(format!(
                "@vocal_harmony '{}': {} at {:.3}s would move {:+.0} cents; a harmony stays within ±{:.0} cents of the lead",
                harmony.name,
                pitch_name(lead_cents),
                note.start_s,
                shift_cents,
                MAX_INTERVAL_CENTS
            ));
        }
        plan.notes.push(HarmonyNote {
            detected_index: index,
            start_s: note.start_s,
            end_s: note.end_s,
            lead_cents,
            target_cents,
            shift_cents,
        });
        Ok(())
    };
    for (index, note) in analysis.analysis.notes.iter().enumerate() {
        // A low-confidence note has no reliable pitch to transpose.
        if note.confidence < crate::vocal_tune::CONFIDENCE_FLOOR {
            continue;
        }
        if note.end_s <= note.start_s {
            continue;
        }
        voice(note, index)?;
    }
    if plan.chromatic && !plan.notes.is_empty() {
        plan.warnings.push(format!(
            "no scale is active for clip '{}'; interval {} counts chromatic semitones",
            clip.name, harmony.interval
        ));
    }
    Ok(plan)
}

/// The plan as one shift curve over the clip's detected note spans.
///
/// Each segment is one note's span with the voice's own glide, the same shape
/// the tune pass builds, so a note ramps into its shift at the head.
pub fn harmony_curve(
    frames: usize,
    sample_rate: u32,
    plan: &HarmonyPlan,
    tune: Option<&VocalTune>,
) -> Result<ShiftCurve, String> {
    let rate = f64::from(sample_rate);
    let ramp = (plan.retune_seconds(tune).max(0.0) * rate).round() as usize;
    let mut segments = Vec::with_capacity(plan.notes.len());
    let mut cursor = 0usize;
    for note in &plan.notes {
        let start = ((note.start_s * rate).round().max(0.0) as usize).max(cursor);
        let end = (note.end_s * rate).round().max(0.0) as usize;
        let end = end.min(frames);
        if end <= start + 1 {
            continue;
        }
        cursor = end;
        segments.push((
            ShiftSegment {
                start,
                end,
                cents: note.shift_cents as f32,
                formant_cents: 0.0,
            },
            Some(ramp),
        ));
    }
    ShiftCurve::from_ramped_segments(frames, &segments)
        .map_err(|error| format!("harmony shift curve: {error}"))
}

/// RMS of the given frame spans over both channels.
fn spans_rms(samples: &[f32], spans: &[(usize, usize)]) -> f64 {
    let mut energy = 0.0_f64;
    let mut count = 0usize;
    for (start, end) in spans {
        for frame in *start..*end {
            for channel in 0..2 {
                let value = f64::from(samples[frame * 2 + channel]);
                energy += value * value;
                count += 1;
            }
        }
    }
    if count == 0 {
        return 0.0;
    }
    (energy / count as f64).sqrt()
}

/// Frame spans the voice covers, one per resolved note.
fn note_spans(plan: &HarmonyPlan, frames: usize, sample_rate: u32) -> Vec<(usize, usize)> {
    let rate = f64::from(sample_rate);
    plan.notes
        .iter()
        .filter_map(|note| {
            let start = (note.start_s * rate).round().max(0.0) as usize;
            let end = (note.end_s * rate).round().max(0.0) as usize;
            let end = end.min(frames);
            (end > start + 1).then_some((start, end))
        })
        .collect()
}

/// Scale the voice so its RMS over the notes sits `level_db` below the lead's.
///
/// One gain for the whole voice, not one per note: a per-note trim would step
/// the level at every boundary. A silent voice is left silent.
fn level_gain(lead: &[f32], voice: &[f32], spans: &[(usize, usize)], level_db: f64) -> f32 {
    let lead_rms = spans_rms(lead, spans);
    let voice_rms = spans_rms(voice, spans);
    if lead_rms <= 1e-9 || voice_rms <= 1e-9 {
        return 0.0;
    }
    let gain = 10f64.powf(level_db / 20.0) * lead_rms / voice_rms;
    gain as f32
}

/// Identity of one harmony bake: the lead's bake identity plus the voice fields.
#[derive(Debug, Clone)]
pub struct HarmonyKey {
    pub lead: crate::vocal_tune::BakeKey,
    pub voice: u64,
    pub version: u32,
}

impl HarmonyKey {
    /// Key for a lead bake and harmony voice.
    pub fn new(lead: crate::vocal_tune::BakeKey, harmony: &VocalHarmony) -> Self {
        Self {
            lead,
            voice: oc_dsp::vocal_analysis::fnv1a(voice_stamp(harmony).as_bytes()),
            version: HARMONY_VERSION,
        }
    }

    /// Stable key string.
    pub fn stamp(&self) -> String {
        format!("{}|{:016x}|{}", self.lead.stamp(), self.voice, self.version)
    }

    /// Cache file name for a clip.
    pub fn file_name(&self, clip: &str) -> String {
        format!(
            "{}-harmony-{:016x}.wav",
            crate::vocal_tune::sanitize_file_stem(clip),
            oc_dsp::vocal_analysis::fnv1a(self.stamp().as_bytes())
        )
    }

    /// Cache directory shared with the lead bake.
    pub fn dir_name(&self) -> String {
        self.lead.dir_name()
    }
}

/// The fields that shape the baked voice. `pan` is a mix-time balance, so it is
/// not here: moving the voice in the image must not rename its bake.
pub fn voice_stamp(harmony: &VocalHarmony) -> String {
    format!(
        "harmony={};interval={};level={:016x}",
        harmony.name,
        harmony.interval,
        harmony.level_db.to_bits()
    )
}

/// Directory for render-time harmony bakes, next to the analysis cache.
fn shared_path(key: &HarmonyKey, clip: &str) -> Option<PathBuf> {
    crate::vocal_analysis::default_cache_dir().map(|dir| {
        dir.join("bake")
            .join(key.dir_name())
            .join(key.file_name(clip))
    })
}

/// Bake one harmony voice for the render path, or `None` when the clip has no
/// harmony block.
///
/// `lead` is the clip's tuned window and its analysis key when a `@vocal_tune`
/// block already produced them: the voice follows the tuned lead and reuses its
/// identity. A cache hit skips the analysis and the shift.
pub fn render_harmony_window(
    project: &Project,
    clip: &AudioClip,
    sample_rate: u32,
    root: Option<&Path>,
    lead: Option<(&VocalAnalysisKey, &[f32])>,
) -> Result<Option<Vec<f32>>, String> {
    let Some(harmony) = harmony_for_clip(project, clip) else {
        return Ok(None);
    };
    let owned: Vec<f32>;
    let (key, window): (VocalAnalysisKey, &[f32]) = match lead {
        Some((key, window)) => (key.clone(), window),
        None => {
            let (key, window) =
                crate::vocal_analysis::clip_stereo_playback(project, clip, sample_rate, root)?;
            owned = window;
            (key, &owned)
        }
    };
    let tune = tune_for_clip(project, clip);
    // A project with no key at all is chromatic whatever the analysis says, so
    // say so on every load, not only on a bake miss.
    if tune.and_then(|tune| tune.scale).is_none()
        && project.properties.key.is_none()
        && project.properties.key_map.is_empty()
    {
        crate::render_warnings::push(format!(
            "@vocal_harmony '{}': no scale is active for clip '{}'; interval {} counts chromatic semitones",
            harmony.name, clip.name, harmony.interval
        ));
    }
    let bake_key = HarmonyKey::new(
        crate::vocal_tune::BakeKey::new(key, tune.unwrap_or(&VocalTune::default()), None),
        harmony,
    );
    let frames = window.len() / 2;
    let cached = shared_path(&bake_key, &clip.name);
    if let Some(path) = &cached
        && let Some(samples) = crate::vocal_tune::read_baked(path, frames, sample_rate)
    {
        return Ok(Some(samples));
    }
    let analysis = crate::vocal_analysis::analyze_clip(
        project,
        clip,
        sample_rate,
        root,
        &oc_dsp::vocal_analysis::DetectorParams::default(),
        None,
    )?;
    let plan = resolve_harmony(project, clip, harmony, &analysis)?;
    for warning in &plan.warnings {
        crate::render_warnings::push(format!("@vocal_harmony '{}': {warning}", harmony.name));
    }
    let curve = harmony_curve(frames, sample_rate, &plan, tune)?;
    let mut config = ShiftConfig::new(sample_rate);
    config.ramp_frames =
        (plan.retune_seconds(tune).max(0.0) * f64::from(sample_rate)).round() as usize;
    let output = vocal_shift::shift(window, 2, sample_rate, &curve, &config)
        .map_err(|error| format!("@vocal_harmony '{}': {error}", harmony.name))?;
    let spans = note_spans(&plan, frames, sample_rate);
    let gain = level_gain(window, &output.samples, &spans, harmony.level_db);
    let mut samples = output.samples;
    for sample in &mut samples {
        *sample *= gain;
    }
    if let Some(path) = &cached
        && let Some(parent) = path.parent()
    {
        let _ = fs::create_dir_all(parent);
        let _ = crate::vocal_tune::write_baked_wav(path, &samples, sample_rate);
    }
    Ok(Some(samples))
}

/// True for a window whose two channels carry the same signal.
pub fn is_mono(window: &[f32]) -> bool {
    window.chunks_exact(2).all(|frame| frame[0] == frame[1])
}

/// Channel gains for one voice.
///
/// A mono source pans with the constant-power law normalised to unity at the
/// centre; a source that already carries a stereo image is balanced, matching
/// the engine's send law.
pub fn pan_gains(pan: f64, mono: bool) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    if pan == 0.0 {
        return (1.0, 1.0);
    }
    if mono {
        let angle = (pan + 1.0) * std::f64::consts::FRAC_PI_4;
        (
            (std::f64::consts::SQRT_2 * angle.cos()) as f32,
            (std::f64::consts::SQRT_2 * angle.sin()) as f32,
        )
    } else {
        (
            (1.0 - pan).clamp(0.0, 1.0) as f32,
            (1.0 + pan).clamp(0.0, 1.0) as f32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn major() -> KeySignature {
        KeySignature {
            root: oc_dsl::ast::NoteName::C,
            accidental: oc_dsl::ast::Accidental::Natural,
            mode: oc_dsl::ast::KeyMode::Major,
        }
    }

    #[test]
    fn a_third_above_c_is_e_and_above_d_is_f() {
        let scale = major();
        let c4 = 6_000.0;
        let d4 = 6_200.0;
        assert_eq!(diatonic_target(&scale, c4, 2), Some(6_400.0));
        assert_eq!(diatonic_target(&scale, d4, 2), Some(6_500.0));
        assert_eq!(diatonic_target(&scale, c4, 1), Some(6_200.0));
        assert_eq!(diatonic_target(&scale, c4, -1), Some(5_900.0));
        assert_eq!(diatonic_target(&scale, c4, 7), Some(7_200.0));
    }

    #[test]
    fn a_note_off_the_scale_snaps_before_it_steps() {
        let scale = major();
        // 30 cents sharp of C4 still counts as the first degree.
        assert_eq!(diatonic_target(&scale, 6_030.0, 2), Some(6_400.0));
        // B4 is the seventh degree; a third above leaves the grid below.
        assert_eq!(diatonic_target(&scale, 7_100.0, 2), Some(7_400.0));
    }

    #[test]
    fn the_pan_is_unity_at_the_centre_and_constant_power_for_mono() {
        assert_eq!(pan_gains(0.0, true), (1.0, 1.0));
        assert_eq!(pan_gains(0.0, false), (1.0, 1.0));
        let (left, right) = pan_gains(-1.0, true);
        assert!(left > 1.4 && left < 1.5, "{left}");
        assert!(right.abs() < 1e-6, "{right}");
        assert_eq!(pan_gains(0.5, false), (0.5, 1.0));
        assert_eq!(pan_gains(-0.5, false), (1.0, 0.5));
    }

    #[test]
    fn a_mono_window_is_detected_from_equal_channels() {
        assert!(is_mono(&[0.1, 0.1, -0.2, -0.2]));
        assert!(!is_mono(&[0.1, 0.2]));
    }

    #[test]
    fn the_voice_stamp_covers_the_fields_that_shape_the_bake() {
        let base = VocalHarmony {
            name: "third".into(),
            clip: Some("lead".into()),
            track: None,
            interval: 2,
            level_db: -6.0,
            pan: 0.0,
        };
        let mut louder = base.clone();
        louder.level_db = -3.0;
        let mut panned = base.clone();
        panned.pan = -0.5;
        let mut wider = base.clone();
        wider.interval = 3;
        assert_ne!(voice_stamp(&base), voice_stamp(&louder));
        assert_ne!(voice_stamp(&base), voice_stamp(&wider));
        assert_eq!(voice_stamp(&base), voice_stamp(&panned));
    }
}
