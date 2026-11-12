//! Note-level transcription for the agent surface: the shared monophonic
//! detector, equal-temperament naming, a conservative polyphony flag and a
//! beat grid a duration token is derived from.
//!
//! `oc_dsp::vocal_analysis` does the pitch tracking; this module only slices a
//! WAV, names the notes, places them on a constant grid and decides whether a
//! single-voice reading is safe. Offline only.

use std::f64::consts::TAU;
use std::path::Path;

use oc_dsp::vocal_analysis::{self, DetectorParams};
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use serde::Serialize;

use crate::analysis::read_wav;

/// Notes returned when `max_notes` is omitted.
pub const DEFAULT_MAX_NOTES: usize = 256;
/// Hard cap on `max_notes`.
pub const NOTE_LIMIT: usize = 1024;

/// One grid step, a 1/16 note in quarter-note beats. Durations are rounded to
/// this before a duration token is chosen.
pub const QUANTISE_STEP_BEATS: f64 = 0.25;
/// How far off the 1/16 grid a reading may sit and still be called a grid
/// reading: a 64th note, a quarter of one grid step. Past it the reported
/// position and the token are a guess, and the report says so.
pub const QUANTISE_TOLERANCE_BEATS: f64 = 0.0625;

const POLYPHONY_FFT: usize = 2048;
const POLYPHONY_MAX_HZ: f64 = 4000.0;
/// Peaks weaker than this fraction of the strongest (about -16 dB) are ignored.
const POLYPHONY_LEVEL: f32 = 0.15;
/// A higher peak within this fraction of a whole multiple is a harmonic.
const HARMONIC_TOLERANCE: f64 = 0.03;

/// How far one octave has to beat another on its own fundamental before the
/// octave is decided, in dB. Below it the note is reported `octave_ambiguous`
/// and a repeated pitch keeps its earlier octave; above it the detector's
/// octave is replaced by the stronger one. A sub-octave or octave-up layer puts
/// real energy at both f/2 and 2f, so a few dB of difference is a layer, not
/// the note.
pub const OCTAVE_MARGIN_DB: f64 = 6.0;
/// The longest slice of one note the octave evidence listens to.
const OCTAVE_SAMPLE_SECONDS: f64 = 0.20;
/// Below this many samples a segment carries no usable spectrum.
const OCTAVE_MIN_SAMPLES: usize = 32;
/// A repeated pitch keeps the earlier octave only when the next onset is within
/// this many quarter-note beats of the previous note's end.
const OCTAVE_CONTINUITY_BEATS: f64 = 2.0;

/// The constant beat grid the beat fields assume. Beats are quarter notes, as
/// everywhere else in the DSL, so a 6/8 bar is three beats, not six.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeatGrid {
    /// Quarter notes per minute.
    pub tempo_bpm: f64,
    /// Meter numerator, e.g. 3 in 3/4.
    pub numerator: u8,
    /// Meter denominator, e.g. 4 in 3/4.
    pub denominator: u8,
}

impl Default for BeatGrid {
    /// The engine default: whatever a project with no `tempo`/`time_signature`
    /// gets, so the tool and the renderer cannot drift apart.
    fn default() -> Self {
        let properties = oc_dsl::ast::ProjectProperties::default();
        Self {
            tempo_bpm: properties.tempo,
            numerator: properties.time_signature.numerator,
            denominator: properties.time_signature.denominator,
        }
    }
}

impl BeatGrid {
    /// Reject a grid the engine's clock would reject.
    pub fn validate(&self) -> Result<(), String> {
        if !self.tempo_bpm.is_finite() || !(1.0..=400.0).contains(&self.tempo_bpm) {
            return Err(format!(
                "tempo must be inside 1..=400 BPM, got {}",
                self.tempo_bpm
            ));
        }
        if !(1..=32).contains(&self.numerator) {
            return Err(format!(
                "time signature numerator must be inside 1..=32, got {}",
                self.numerator
            ));
        }
        if !matches!(self.denominator, 1 | 2 | 4 | 8 | 16 | 32 | 64) {
            return Err(format!(
                "time signature denominator must be one of 1, 2, 4, 8, 16, 32, 64, got {}",
                self.denominator
            ));
        }
        Ok(())
    }

    /// Quarter-note beats in one bar.
    pub fn beats_per_bar(&self) -> f64 {
        f64::from(self.numerator) * 4.0 / f64::from(self.denominator)
    }

    /// Seconds per quarter-note beat.
    pub fn beat_seconds(&self) -> f64 {
        60.0 / self.tempo_bpm
    }

    /// The meter as the DSL writes it, e.g. `3/4`.
    pub fn time_signature(&self) -> String {
        format!("{}/{}", self.numerator, self.denominator)
    }
}

/// A window and the two filters the tool exposes.
#[derive(Debug, Clone, Copy)]
pub struct NoteOptions {
    /// Inclusive window start in seconds; `None` means the file start.
    pub start_s: Option<f64>,
    /// Exclusive window end in seconds; `None` means the file end.
    pub end_s: Option<f64>,
    /// Drop notes the detector scored below this.
    pub min_confidence: f64,
    /// Cap on returned notes, earliest first; `None` uses [`DEFAULT_MAX_NOTES`].
    pub max_notes: Option<usize>,
    /// The grid the beat fields assume.
    pub grid: BeatGrid,
    /// Snap the reported beat values to the 1/16 grid as well as reporting raw.
    pub quantize: bool,
}

impl Default for NoteOptions {
    fn default() -> Self {
        Self {
            start_s: None,
            end_s: None,
            min_confidence: 0.0,
            max_notes: None,
            grid: BeatGrid::default(),
            quantize: false,
        }
    }
}

/// One transcribed note. Times are relative to the analysed window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoteEvent {
    pub start_s: f64,
    pub end_s: f64,
    pub midi: i32,
    pub name: String,
    pub cents: f64,
    pub confidence: f64,
    /// Quarter-note beats from the window's musical start, 0-based.
    pub start_beat: f64,
    /// Measured length in quarter-note beats.
    pub duration_beats: f64,
    /// 1-based bar of the 1/16-snapped start: a bar is a grid label, so a
    /// measurement a few milliseconds before a bar line still lands in the
    /// right bar.
    pub bar: u32,
    /// The nearest DSL duration token to the 1/16-rounded length.
    pub suggested_duration: String,
    /// Snapped start, present only when `quantize` is on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_beat_quantised: Option<f64>,
    /// Snapped length, present only when `quantize` is on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_beats_quantised: Option<f64>,
    /// True when two octaves of this pitch explain the note's own spectrum
    /// within [`OCTAVE_MARGIN_DB`], or when the segment was too quiet to
    /// measure. The octave is the stronger reading, not a measurement: ask for
    /// a window where the voice sounds alone.
    pub octave_ambiguous: bool,
    /// The next-best octave candidate, as a MIDI number, whenever the note's
    /// own spectrum was measurable. It is the octave the note would carry if
    /// the ambiguity were resolved the other way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub octave_runner_up: Option<i32>,
}

/// The tool payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoteReport {
    pub path: String,
    /// Window start in file time, as requested.
    pub start_s: f64,
    /// Exclusive window end in file time, clamped to the file duration.
    pub end_s: f64,
    pub analysis_rate_hz: f64,
    pub hop_s: f64,
    /// The grid the beat fields assume, as chosen by the caller.
    pub tempo_bpm: f64,
    /// The meter as the DSL writes it, e.g. `3/4`.
    pub time_signature: String,
    /// Quarter-note beats in one bar.
    pub beats_per_bar: f64,
    /// Seconds per quarter-note beat.
    pub beat_seconds: f64,
    /// Tolerance in beats beyond which a reading is called off the grid.
    pub quantise: f64,
    pub notes: Vec<NoteEvent>,
    pub polyphonic: bool,
    pub warnings: Vec<String>,
}

/// Analyse one WAV window into note events. A `start_s`/`end_s` pair slices
/// before the detector runs, so every note time is relative to the window.
pub fn analyze_notes(path: impl AsRef<Path>, options: &NoteOptions) -> Result<NoteReport, String> {
    let path = path.as_ref();
    let buffer =
        read_wav(path).map_err(|error| format!("could not read '{}': {error}", path.display()))?;
    let rate = buffer.sample_rate;
    let duration_s = buffer.duration_s();
    let start_s = options.start_s.unwrap_or(0.0);
    let end_s = options.end_s.unwrap_or(duration_s);
    if !start_s.is_finite() || !end_s.is_finite() {
        return Err("window bounds must be finite seconds".into());
    }
    if start_s < 0.0 {
        return Err(format!("start must be at least 0 s, got {start_s}"));
    }
    if end_s <= start_s {
        return Err(format!(
            "end must be after start: start {start_s} s, end {end_s} s"
        ));
    }
    if !options.min_confidence.is_finite() || !(0.0..=1.0).contains(&options.min_confidence) {
        return Err(format!(
            "min_confidence must be inside 0..=1, got {}",
            options.min_confidence
        ));
    }
    let max_notes = options.max_notes.unwrap_or(DEFAULT_MAX_NOTES);
    if max_notes == 0 || max_notes > NOTE_LIMIT {
        return Err(format!(
            "max_notes must be inside 1..={NOTE_LIMIT}, got {max_notes}"
        ));
    }
    let grid = options.grid;
    grid.validate()?;
    let beat_seconds = grid.beat_seconds();
    let beats_per_bar = grid.beats_per_bar();

    let mut warnings: Vec<String> = Vec::new();
    let end_s = end_s.min(duration_s);
    let first = ((start_s * f64::from(rate)).round() as usize).min(buffer.frames());
    let last = ((end_s * f64::from(rate)).round() as usize)
        .min(buffer.frames())
        .max(first);
    let mono = buffer.mono();
    let window: Vec<f32> = mono[first..last]
        .iter()
        .map(|value| *value as f32)
        .collect();

    if window.is_empty() {
        warnings.push(format!(
            "the window is empty: start {start_s} s is at or past the file end {duration_s} s"
        ));
        return Ok(NoteReport {
            path: path.display().to_string(),
            start_s,
            end_s,
            analysis_rate_hz: 0.0,
            hop_s: 0.0,
            tempo_bpm: grid.tempo_bpm,
            time_signature: grid.time_signature(),
            beats_per_bar,
            beat_seconds,
            quantise: QUANTISE_TOLERANCE_BEATS,
            notes: Vec::new(),
            polyphonic: false,
            warnings,
        });
    }

    let analysis = vocal_analysis::analyze(&window, rate, &DetectorParams::default())
        .map_err(|error| format!("note detector refused the window: {error}"))?;
    let polyphonic = window_is_polyphonic(&window, rate);
    if polyphonic {
        warnings.push(
            "the window may contain more than one sounding pitch; this is a single-voice detector, \
             it transcribed only the strongest voice and the remaining voices are not transcribed"
                .into(),
        );
    }

    // Off-grid readings are collected by returned index, so the warning can
    // point at the array the caller sees after drops and truncation.
    let mut off_grid_starts: Vec<usize> = Vec::new();
    let mut rough_tokens: Vec<usize> = Vec::new();
    let mut notes: Vec<NoteEvent> = Vec::new();
    let mut below = 0usize;
    // The last octave of each pitch class and where that note ended, so a
    // repeated pitch can hold its octave through an ambiguous reading.
    let mut latest: [Option<(f64, i32)>; 12] = [None; 12];
    for note in &analysis.notes {
        if note.confidence < options.min_confidence {
            below += 1;
            continue;
        }
        let index = notes.len();
        let (detected_midi, cents) = name_frequency(note.f0_median_hz);
        let octave = octave_evidence(&window, rate, note.start_s, note.end_s, detected_midi);
        let (mut midi, octave_ambiguous, mut octave_runner_up) = match &octave {
            Some(choice) => (
                choice.midi,
                choice.margin_db < OCTAVE_MARGIN_DB,
                choice.runner_up,
            ),
            // No measurable spectrum: the detector's octave stands, but it is
            // not confirmed by the note's own harmonics.
            None => (detected_midi, true, None),
        };
        let start_beat = note.start_s / beat_seconds;
        let duration_beats = (note.end_s - note.start_s) / beat_seconds;
        // Hold the octave of a repeated pitch only while the previous one is
        // still one of this note's own candidates: across two octaves it is a
        // different voice, not a flip.
        let class = midi.rem_euclid(12) as usize;
        if octave_ambiguous
            && let Some((previous_end_beat, previous_midi)) = latest[class]
            && start_beat - previous_end_beat <= OCTAVE_CONTINUITY_BEATS
            && (previous_midi - midi).abs() == 12
        {
            octave_runner_up = Some(midi);
            midi = previous_midi;
        }
        if start_beat >= 0.0 {
            latest[class] = Some((start_beat + duration_beats, midi));
        }
        let start_beat_quantised = quantise_beats(start_beat);
        let duration_beats_quantised = quantise_beats(duration_beats).max(QUANTISE_STEP_BEATS);
        // The token comes from the 1/16-rounded length even when the reported
        // beats stay raw: a token is a grid object, not a measurement.
        let (suggested_duration, token_beats) = duration_token_for(duration_beats_quantised);
        if (start_beat - start_beat_quantised).abs() > QUANTISE_TOLERANCE_BEATS {
            off_grid_starts.push(index);
        }
        if (duration_beats_quantised - token_beats).abs() > QUANTISE_TOLERANCE_BEATS {
            rough_tokens.push(index);
        }
        notes.push(NoteEvent {
            start_s: note.start_s,
            end_s: note.end_s,
            midi,
            name: note_name(midi),
            cents,
            confidence: note.confidence,
            start_beat,
            duration_beats,
            bar: bar_of(start_beat_quantised, beats_per_bar),
            suggested_duration: suggested_duration.to_owned(),
            start_beat_quantised: options.quantize.then_some(start_beat_quantised),
            duration_beats_quantised: options.quantize.then_some(duration_beats_quantised),
            octave_ambiguous,
            octave_runner_up,
        });
    }
    let ambiguous = notes.iter().filter(|note| note.octave_ambiguous).count();
    if ambiguous > 0 {
        warnings.push(format!(
            "{ambiguous} of {} notes are octave-ambiguous: two octaves explain the note's own \
             spectrum within {OCTAVE_MARGIN_DB} dB, or the segment was too quiet to measure \
             (octave_ambiguous=true; octave_runner_up is the other octave when there was one). \
             The octave is the stronger reading, not a measurement — transcribe a window where \
             that voice sounds alone before trusting it",
            notes.len()
        ));
    }
    if below > 0 {
        warnings.push(format!(
            "{below} notes were below min_confidence={} and were dropped",
            options.min_confidence
        ));
    }
    if notes.len() > max_notes {
        let omitted = notes.len() - max_notes;
        notes.truncate(max_notes);
        warnings.push(format!(
            "{omitted} notes were omitted: max_notes={max_notes} keeps the earliest first"
        ));
        off_grid_starts.retain(|index| *index < max_notes);
        rough_tokens.retain(|index| *index < max_notes);
    }
    if !off_grid_starts.is_empty() {
        warnings.push(format!(
            "{} of {} notes start more than {QUANTISE_TOLERANCE_BEATS} beats (a 64th) off the 1/16 \
             grid (0-based array indices {}); their start_beat, bar and suggested_duration are the \
             nearest reading, not a measurement",
            off_grid_starts.len(),
            notes.len(),
            index_list(&off_grid_starts)
        ));
    }
    if !rough_tokens.is_empty() {
        warnings.push(format!(
            "{} of {} notes have no DSL duration token within {QUANTISE_TOLERANCE_BEATS} beats of \
             the 1/16-rounded length (0-based array indices {}); suggested_duration is the nearest \
             token and is a guess — check it by ear or write a tie",
            rough_tokens.len(),
            notes.len(),
            index_list(&rough_tokens)
        ));
    }
    if notes.is_empty() {
        warnings.push(
            "the window has no notes: nothing crossed the detector's silence gate or its minimum \
             note length, and drums, chords and ornaments under about 50 ms are not transcribed"
                .into(),
        );
    }

    Ok(NoteReport {
        path: path.display().to_string(),
        start_s,
        end_s,
        analysis_rate_hz: analysis.analysis_rate_hz,
        hop_s: analysis.hop_seconds,
        tempo_bpm: grid.tempo_bpm,
        time_signature: grid.time_signature(),
        beats_per_bar,
        beat_seconds,
        quantise: QUANTISE_TOLERANCE_BEATS,
        notes,
        polyphonic,
        warnings,
    })
}

/// The nearest 1/16-grid point, in quarter-note beats.
fn quantise_beats(beats: f64) -> f64 {
    (beats / QUANTISE_STEP_BEATS).round() * QUANTISE_STEP_BEATS
}

/// 1-based bar of a beat position; positions before the window start clamp to
/// bar 1.
fn bar_of(start_beat: f64, beats_per_bar: f64) -> u32 {
    (start_beat.max(0.0) / beats_per_bar).floor() as u32 + 1
}

/// The `0, 2, 5` list in a warning, bounded so a sloppy take cannot flood it.
fn index_list(indices: &[usize]) -> String {
    const SHOWN: usize = 12;
    let mut text = indices
        .iter()
        .take(SHOWN)
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if indices.len() > SHOWN {
        text.push_str(", …");
    }
    text
}

/// The DSL duration tokens a single note can carry, simplest first so a tie
/// resolves to the plain form. The grammar has no double dot and the dotted
/// triplet forms all duplicate a plain length, so one dot and a triplet are the
/// whole vocabulary.
const DURATION_TOKENS: [(&str, f64); 21] = [
    ("w", 4.0),
    ("h", 2.0),
    ("q", 1.0),
    ("8th", 0.5),
    ("16th", 0.25),
    ("32nd", 0.125),
    ("64th", 0.0625),
    ("w.", 6.0),
    ("h.", 3.0),
    ("q.", 1.5),
    ("8th.", 0.75),
    ("16th.", 0.375),
    ("32nd.", 0.1875),
    ("64th.", 0.09375),
    ("wt", 8.0 / 3.0),
    ("ht", 4.0 / 3.0),
    ("qt", 2.0 / 3.0),
    ("8tht", 1.0 / 3.0),
    ("16tht", 1.0 / 6.0),
    ("32ndt", 1.0 / 12.0),
    ("64tht", 1.0 / 24.0),
];

/// The token nearest `beats`, with its length. `beats` is expected to be on the
/// 1/16 grid; the caller decides whether the distance is acceptable.
fn duration_token_for(beats: f64) -> (&'static str, f64) {
    let mut best = DURATION_TOKENS[0];
    let mut best_gap = (best.1 - beats).abs();
    for token in &DURATION_TOKENS[1..] {
        let gap = (token.1 - beats).abs();
        if gap < best_gap {
            best = *token;
            best_gap = gap;
        }
    }
    best
}

/// How the note's own spectrum votes on its octave.
struct OctaveChoice {
    /// The octave the note is reported in.
    midi: i32,
    /// The strongest other octave, when there is one.
    runner_up: Option<i32>,
    /// Level gap in dB between the chosen octave and its best alternative.
    margin_db: f64,
}

/// Which octave of `midi` the note is sounding in. A tone with a sub-octave or
/// octave-up layer carries energy at f/2 and 2f as well as f, so the detector's
/// period can land on any of them; the line at each candidate's own fundamental
/// says which octave is sounding. The detector's octave is kept unless another
/// octave's own fundamental wins by [`OCTAVE_MARGIN_DB`], and the remaining gap
/// is the report's octave confidence.
fn octave_evidence(
    window: &[f32],
    rate: u32,
    start_s: f64,
    end_s: f64,
    midi: i32,
) -> Option<OctaveChoice> {
    let rate = f64::from(rate);
    let first = (start_s.max(0.0) * rate).round() as usize;
    let last = ((end_s.max(0.0) * rate).round() as usize).min(window.len());
    if last <= first {
        return None;
    }
    let span = last - first;
    let cap = (OCTAVE_SAMPLE_SECONDS * rate).round() as usize;
    let take = span.min(cap.max(OCTAVE_MIN_SAMPLES));
    let middle = first + span / 2;
    let low = middle.saturating_sub(take / 2).max(first);
    let high = (low + take).min(last);
    if high - low < OCTAVE_MIN_SAMPLES {
        return None;
    }
    let segment = &window[low..high];
    let hann: Vec<f64> = (0..segment.len())
        .map(|index| 0.5 - 0.5 * (TAU * index as f64 / (segment.len() - 1) as f64).cos())
        .collect();
    let weight: f64 = hann.iter().sum();
    if weight <= 0.0 {
        return None;
    }
    let rms = (segment
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>()
        / segment.len() as f64)
        .sqrt();
    // A segment at the detector's silence gate carries noise, not a note: the
    // amplitude ratios below would be meaningless, so the octave is left
    // unconfirmed instead of guessed from rounding.
    if rms < DetectorParams::default().silence_rms * 2.0 {
        return None;
    }
    let mut candidates: Vec<(i32, f64)> = [midi - 12, midi, midi + 12]
        .into_iter()
        .filter(|value| (0..=127).contains(value))
        .map(|value| {
            let frequency = 440.0 * 2f64.powf((f64::from(value) - 69.0) / 12.0);
            (
                value,
                tone_amplitude(segment, &hann, weight, rate, frequency),
            )
        })
        .collect();
    candidates.sort_by(|left, right| right.1.total_cmp(&left.1));
    let (strongest_midi, strongest) = candidates.first().copied()?;
    if strongest.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    // The detector's octave stays unless another octave's own fundamental wins
    // by the full override margin; a partial an octave up is a harmonic, not a
    // note. Without that prior the loudest harmonic would set the octave.
    let detected = candidates.iter().find(|(candidate, _)| *candidate == midi);
    let (chosen, chosen_amplitude) = match detected {
        Some((_, detected_amplitude)) => {
            let advantage = 20.0 * (strongest / detected_amplitude.max(f64::MIN_POSITIVE)).log10();
            if strongest_midi != midi && advantage >= OCTAVE_MARGIN_DB {
                (strongest_midi, strongest)
            } else {
                (midi, *detected_amplitude)
            }
        }
        None => (strongest_midi, strongest),
    };
    let best_alternative = candidates
        .iter()
        .filter(|(candidate, _)| *candidate != chosen)
        .map(|(_, amplitude)| *amplitude)
        .fold(0.0, f64::max);
    let runner_up = candidates
        .iter()
        .find(|(candidate, _)| *candidate != chosen)
        .map(|(candidate, _)| *candidate);
    let margin_db = 20.0
        * (chosen_amplitude / best_alternative.max(f64::MIN_POSITIVE))
            .log10()
            .abs();
    Some(OctaveChoice {
        midi: chosen,
        runner_up,
        margin_db,
    })
}

/// Amplitude of the spectral line at `frequency_hz`: a Hann-windowed DFT
/// evaluated at exactly that frequency with a rotating phasor. Evaluating off
/// the bin grid keeps the estimate amplitude-true, which a peak bin does not.
fn tone_amplitude(segment: &[f32], hann: &[f64], weight: f64, rate: f64, frequency_hz: f64) -> f64 {
    let (sin_step, cos_step) = (TAU * frequency_hz / rate).sin_cos();
    let (mut cos, mut sin) = (1.0f64, 0.0f64);
    let (mut real, mut imaginary) = (0.0f64, 0.0f64);
    for (sample, window) in segment.iter().zip(hann) {
        let value = f64::from(*sample) * window;
        real += value * cos;
        imaginary -= value * sin;
        let next_cos = cos * cos_step - sin * sin_step;
        sin = sin * cos_step + cos * sin_step;
        cos = next_cos;
    }
    2.0 * (real * real + imaginary * imaginary).sqrt() / weight
}

/// Equal-tempered MIDI number and cents off that pitch, A4 = 440 Hz.
pub fn name_frequency(frequency_hz: f64) -> (i32, f64) {
    let midi = (69.0 + 12.0 * (frequency_hz / 440.0).log2()).round();
    let reference = 440.0 * 2f64.powf((midi - 69.0) / 12.0);
    let cents = 1200.0 * (frequency_hz / reference).log2();
    (midi as i32, cents)
}

/// Scientific pitch name with sharps, e.g. `A4` or `C#5`.
pub fn note_name(midi: i32) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let index = midi.rem_euclid(12) as usize;
    format!("{}{}", NAMES[index], midi.div_euclid(12) - 1)
}

/// Whether at least half the window's frames carry two independent strong
/// pitches, where a partial explained by a lower peak is not independent.
fn window_is_polyphonic(samples: &[f32], sample_rate: u32) -> bool {
    if sample_rate == 0 || samples.len() < POLYPHONY_FFT * 2 {
        return false;
    }
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(POLYPHONY_FFT);
    let window: Vec<f32> = (0..POLYPHONY_FFT)
        .map(|index| 0.5 - 0.5 * (TAU * index as f64 / POLYPHONY_FFT as f64).cos() as f32)
        .collect();
    let bin_hz = f64::from(sample_rate) / POLYPHONY_FFT as f64;
    let max_bin = ((POLYPHONY_MAX_HZ / bin_hz) as usize).clamp(4, POLYPHONY_FFT / 2 - 1);
    let hop = POLYPHONY_FFT / 2;
    let mut buffer = vec![Complex::new(0.0, 0.0); POLYPHONY_FFT];
    let mut frames = 0usize;
    let mut two_voice = 0usize;
    let mut offset = 0usize;
    while offset + POLYPHONY_FFT <= samples.len() {
        let mut energy = 0.0f32;
        for index in 0..POLYPHONY_FFT {
            let value = samples[offset + index] * window[index];
            energy += value * value;
            buffer[index] = Complex::new(value, 0.0);
        }
        offset += hop;
        if (energy / POLYPHONY_FFT as f32).sqrt() < 1e-4 {
            continue;
        }
        fft.process(&mut buffer);
        let mut peaks: Vec<(f64, f32)> = Vec::new();
        for bin in 1..max_bin {
            let left = buffer[bin - 1].norm();
            let center = buffer[bin].norm();
            let right = buffer[bin + 1].norm();
            if center > left && center >= right {
                // Parabolic interpolation keeps harmonic ratios inside the
                // tolerance even when the fundamental sits between bins.
                let denominator = left - 2.0 * center + right;
                let delta = if denominator.abs() > 1e-12 {
                    (0.5 * (left - right) / denominator).clamp(-0.5, 0.5)
                } else {
                    0.0
                };
                peaks.push(((bin as f64 + f64::from(delta)) * bin_hz, center));
            }
        }
        let strongest = peaks.iter().map(|peak| peak.1).fold(0.0_f32, f32::max);
        peaks.retain(|peak| peak.1 >= strongest * POLYPHONY_LEVEL);
        // Ascending frequency: a partial is explained by any lower strong peak
        // it is a whole multiple of (or the same pitch, for a detuned unison).
        peaks.sort_by(|left, right| left.0.total_cmp(&right.0));
        let mut voices = 0usize;
        for index in 0..peaks.len() {
            let frequency = peaks[index].0;
            let explained = peaks[..index].iter().any(|lower| {
                let ratio = frequency / lower.0;
                let multiple = ratio.round();
                ratio < 1.06
                    || (multiple >= 2.0 && (ratio / multiple - 1.0).abs() <= HARMONIC_TOLERANCE)
            });
            if !explained {
                voices += 1;
            }
        }
        if voices >= 2 {
            two_voice += 1;
        }
        frames += 1;
    }
    frames >= 4 && two_voice * 2 >= frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_equal_temperament_with_a4_at_69() {
        let (midi, cents) = name_frequency(440.0);
        assert_eq!(midi, 69);
        assert!(cents.abs() < 1e-9);
        assert_eq!(note_name(midi), "A4");
        let (midi, cents) = name_frequency(523.251_130_5);
        assert_eq!(midi, 72);
        assert_eq!(note_name(midi), "C5");
        assert!(cents.abs() < 0.01);
        let (midi, cents) = name_frequency(440.0 * 2f64.powf(20.0 / 1200.0));
        assert_eq!(midi, 69);
        assert!((cents - 20.0).abs() < 1e-6);
        assert_eq!(note_name(0), "C-1");
        assert_eq!(note_name(127), "G9");
    }

    #[test]
    fn a_silent_window_has_no_notes_and_one_readable_warning() {
        let samples = vec![0.0f32; 48_000];
        assert!(!window_is_polyphonic(&samples, 48_000));
    }

    #[test]
    fn two_sines_a_fifth_apart_are_polyphonic_and_one_sine_is_not() {
        let rate = 48_000u32;
        let fifth: Vec<f32> = (0..rate)
            .map(|index| {
                let time = index as f64 / f64::from(rate);
                (0.4 * (TAU * 220.0 * time).sin() + 0.4 * (TAU * 330.0 * time).sin()) as f32
            })
            .collect();
        assert!(window_is_polyphonic(&fifth, rate));
        let single: Vec<f32> = (0..rate)
            .map(|index| (0.5 * (TAU * 440.0 * index as f64 / f64::from(rate)).sin()) as f32)
            .collect();
        assert!(!window_is_polyphonic(&single, rate));
        // A harmonic pair is one voice, not two.
        let harmonic: Vec<f32> = (0..rate)
            .map(|index| {
                let time = index as f64 / f64::from(rate);
                (0.4 * (TAU * 440.0 * time).sin() + 0.2 * (TAU * 880.0 * time).sin()) as f32
            })
            .collect();
        assert!(!window_is_polyphonic(&harmonic, rate));
        // A harmonic-rich single note stays one voice even when its upper
        // partials are the strongest peaks.
        let saw: Vec<f32> = (0..rate)
            .map(|index| {
                let time = index as f64 / f64::from(rate);
                (1..=8)
                    .map(|partial| {
                        (0.4 / partial as f64) * (TAU * 98.0 * partial as f64 * time).sin()
                    })
                    .sum::<f64>() as f32
            })
            .collect();
        assert!(!window_is_polyphonic(&saw, rate));
        // Two harmonic-rich voices a non-integer interval apart are two.
        let two_rich: Vec<f32> = (0..rate)
            .map(|index| {
                let time = index as f64 / f64::from(rate);
                let bass: f64 = (1..=5)
                    .map(|partial| {
                        (0.3 / partial as f64) * (TAU * 98.0 * partial as f64 * time).sin()
                    })
                    .sum();
                let melody: f64 = (1..=5)
                    .map(|partial| {
                        (0.3 / partial as f64) * (TAU * 440.0 * partial as f64 * time).sin()
                    })
                    .sum();
                (bass + melody) as f32
            })
            .collect();
        assert!(window_is_polyphonic(&two_rich, rate));
    }

    /// A boundary is smeared by half the 46 ms analysis window (23 ms): a note
    /// starting at sample 0 loses that half window on its onset, and a trailing
    /// edge adds up to a hop. 40 ms = 23 + 3 x 5 ms hop is the worst case this
    /// file's synthetic notes show, so it is the asserted bound.
    const BOUNDARY_TOLERANCE_S: f64 = 0.040;
    const TEST_RATE: u32 = 48_000;

    /// A mono 16-bit WAV: `(start_s, length_s, hz)` notes with 4 ms fades.
    fn write_phrase(path: &Path, notes: &[(f64, f64, f64)]) {
        let total_s = notes
            .iter()
            .map(|(start, length, _)| start + length)
            .fold(0.0, f64::max)
            + 0.2;
        let frames = (total_s * f64::from(TEST_RATE)).round() as usize;
        let fade = (0.004 * f64::from(TEST_RATE)).round() as usize;
        let mut samples = vec![0.0f64; frames];
        for &(start_s, length_s, hz) in notes {
            let first = (start_s * f64::from(TEST_RATE)).round() as usize;
            let last = ((start_s + length_s) * f64::from(TEST_RATE)).round() as usize;
            for (index, sample) in samples
                .iter_mut()
                .enumerate()
                .take(last.min(frames))
                .skip(first)
            {
                let offset = index - first;
                let remaining = last - index;
                let gain = (offset as f64 / fade as f64)
                    .min(remaining as f64 / fade as f64)
                    .min(1.0);
                let time = offset as f64 / f64::from(TEST_RATE);
                *sample += 0.4 * gain * (TAU * hz * time).sin();
            }
        }
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TEST_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for value in samples {
            writer
                .write_sample((value.clamp(-1.0, 1.0) * f64::from(i16::MAX)) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    fn temp_wav(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("oc-note-grid-{}-{name}.wav", std::process::id()))
    }

    /// A harmonic tone: `partials` are `(multiple of the fundamental, level)`,
    /// each started at `phase * multiple` so a repeat lands on a different
    /// window alignment. A decaying envelope keeps it note-like.
    fn write_harmonic_notes(path: &Path, notes: &[(f64, f64, f64, f64)], partials: &[(f64, f64)]) {
        let total_s = notes
            .iter()
            .map(|(start, length, _, _)| start + length)
            .fold(0.0, f64::max)
            + 0.3;
        let frames = (total_s * f64::from(TEST_RATE)).round() as usize;
        let fade = (0.004 * f64::from(TEST_RATE)).round() as usize;
        let mut samples = vec![0.0f64; frames];
        for &(start_s, length_s, hz, phase) in notes {
            let first = (start_s * f64::from(TEST_RATE)).round() as usize;
            let last = ((start_s + length_s) * f64::from(TEST_RATE)).round() as usize;
            for (index, sample) in samples
                .iter_mut()
                .enumerate()
                .take(last.min(frames))
                .skip(first)
            {
                let offset = index - first;
                let remaining = last - index;
                let gain = (offset as f64 / fade as f64)
                    .min(remaining as f64 / fade as f64)
                    .min(1.0);
                let time = offset as f64 / f64::from(TEST_RATE);
                let mut value = 0.0;
                for &(multiple, level) in partials {
                    value += level * (TAU * hz * multiple * time + phase * multiple).sin();
                }
                *sample += 0.12 * gain * (-4.0 * time).exp() * value;
            }
        }
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TEST_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for value in samples {
            writer
                .write_sample((value.clamp(-1.0, 1.0) * f64::from(i16::MAX)) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    /// The fixture instrument's shape: a harmonic fundamental plus a sub-octave
    /// layer and an octave-up layer, both strong enough that a period-based
    /// detector can land on either.
    const LAYERED_PARTIALS: [(f64, f64); 7] = [
        (0.5, 0.28),
        (1.0, 0.72),
        (2.0, 0.58),
        (3.0, 0.24),
        (4.0, 0.18),
        (5.0, 0.144),
        (6.0, 0.12),
    ];

    #[test]
    fn a_repeated_piano_note_keeps_one_octave_across_phases() {
        let path = temp_wav("repeat-octave");
        let notes: Vec<(f64, f64, f64, f64)> = (0..8)
            .map(|index| {
                let start = index as f64 * 0.52;
                (start, 0.40, 220.0, TAU * index as f64 / 8.0)
            })
            .collect();
        write_harmonic_notes(&path, &notes, &LAYERED_PARTIALS);
        let report = report(&path, grid(120.0, 4, 4), false);
        assert!(report.notes.len() >= 8, "{report:?}");
        let names: Vec<&str> = report.notes.iter().map(|note| note.name.as_str()).collect();
        assert!(
            names.iter().all(|name| *name == "A3"),
            "the octave has to hold across repeats, got {names:?}"
        );
        // The uncertain ones must say so instead of silently guessing.
        for note in &report.notes {
            if let Some(runner) = note.octave_runner_up {
                assert_eq!(
                    (runner - note.midi).abs(),
                    12,
                    "a runner-up has to be the other octave: {note:?}"
                );
            }
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_clean_harmonic_note_is_not_octave_ambiguous() {
        let path = temp_wav("clean-octave");
        write_harmonic_notes(&path, &[(0.0, 0.8, 220.0, 0.0)], &[(1.0, 1.0), (2.0, 0.3)]);
        let report = report(&path, grid(120.0, 4, 4), false);
        assert_eq!(report.notes.len(), 1, "{report:?}");
        let note = &report.notes[0];
        assert_eq!(note.midi, 57, "A3: {report:?}");
        assert!(!note.octave_ambiguous, "{report:?}");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_sub_octave_layer_as_strong_as_the_note_is_flagged_ambiguous() {
        let path = temp_wav("ambiguous-octave");
        // f/2 and f at the same level: one window cannot say which is the note.
        write_harmonic_notes(
            &path,
            &[(0.0, 0.8, 220.0, 0.0)],
            &[(0.5, 0.70), (1.0, 0.70), (2.0, 0.10)],
        );
        let report = report(&path, grid(120.0, 4, 4), false);
        assert_eq!(report.notes.len(), 1, "{report:?}");
        let note = &report.notes[0];
        assert!(note.octave_ambiguous, "{report:?}");
        let runner = note.octave_runner_up.expect("the other octave is reported");
        assert_eq!((runner - note.midi).abs(), 12, "{report:?}");
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("octave-ambiguous")),
            "{report:?}"
        );
        std::fs::remove_file(&path).unwrap();
    }

    fn grid(tempo_bpm: f64, numerator: u8, denominator: u8) -> BeatGrid {
        BeatGrid {
            tempo_bpm,
            numerator,
            denominator,
        }
    }

    fn report(path: &Path, grid: BeatGrid, quantize: bool) -> NoteReport {
        analyze_notes(
            path,
            &NoteOptions {
                grid,
                quantize,
                ..NoteOptions::default()
            },
        )
        .unwrap()
    }

    fn assert_near(actual: f64, expected: f64, tolerance: f64, what: &str) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{what}: {actual} is not within {tolerance} of {expected}"
        );
    }

    #[test]
    fn a_120_bpm_four_four_grid_reports_beats_bars_and_half_notes() {
        let path = temp_wav("120");
        write_phrase(
            &path,
            &[
                (0.0, 1.0, 98.0),   // G2, 2 beats
                (1.0, 1.0, 146.83), // D3, 2 beats
                (2.0, 1.0, 130.81), // C3, 2 beats
                (3.0, 1.0, 98.0),   // G2, 2 beats
            ],
        );
        let report = report(&path, grid(120.0, 4, 4), false);
        let beat_s = report.beat_seconds;
        assert_eq!(report.tempo_bpm, 120.0);
        assert_eq!(report.time_signature, "4/4");
        assert_eq!(report.beats_per_bar, 4.0);
        assert_eq!(report.quantise, QUANTISE_TOLERANCE_BEATS);
        assert_eq!(beat_s, 0.5);
        assert_eq!(report.notes.len(), 4, "{report:?}");
        for (index, (round, bars, token)) in [
            (0.0, 1u32, "h"),
            (2.0, 1, "h"),
            (4.0, 2, "h"),
            (6.0, 2, "h"),
        ]
        .iter()
        .enumerate()
        {
            let note = &report.notes[index];
            assert_near(
                note.start_beat,
                *round,
                BOUNDARY_TOLERANCE_S / beat_s,
                &format!("note {index} start beat"),
            );
            assert_near(
                note.duration_beats,
                2.0,
                BOUNDARY_TOLERANCE_S / beat_s,
                &format!("note {index} duration"),
            );
            assert_eq!(note.bar, *bars, "note {index} bar");
            assert_eq!(note.suggested_duration, *token, "note {index} token");
            assert_eq!(note.start_beat_quantised, None);
        }
        // Every note sits on the grid, so the grid warning stays quiet.
        assert!(
            !report
                .warnings
                .iter()
                .any(|warning| warning.contains("1/16 grid")),
            "{report:?}"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_detected_early_onset_does_not_shorten_the_reported_duration() {
        // The phrase opens after 0.3 s of silence, so the voicing decision
        // flips 0.45 of a window before the first event and that note carries
        // a detected onset well before its reported start. The reported spans
        // stay on the frame grid: a 1 s note reads 1 s and a 2 s note reads
        // 2 s. The two inner notes are bounded by pitch changes on both sides,
        // where the grid is exact; the outer notes carry the grid's own edge
        // smear and are only checked for shrinkage.
        let path = temp_wav("durations");
        write_phrase(
            &path,
            &[
                (0.300, 0.500, 220.0),   // opens the run, detected onset 0.300
                (0.800, 1.000, 277.183), // 1 s
                (1.800, 2.000, 329.628), // 2 s
                (3.800, 0.500, 220.0),   // closes the 2 s note
            ],
        );
        let report = report(&path, grid(120.0, 4, 4), false);
        let beat_s = report.beat_seconds;
        let measured: Vec<f64> = report
            .notes
            .iter()
            .map(|note| note.duration_beats * beat_s)
            .collect();
        assert_eq!(report.notes.len(), 4, "{report:?}");
        // An internal cut sits at the pitch inflection of the transition, which
        // can move it a few frames away from the grid, so a note bounded by
        // pitch changes on both sides may differ from its nominal length by up
        // to 1 %; what must not happen is the systematic shrinkage the
        // frame-edge smear caused (a 1 s note reading 0.929 s).
        for (index, seconds) in [(1usize, 1.0f64), (2, 2.0)] {
            assert!(
                (measured[index] - seconds).abs() <= 0.01 * seconds,
                "note {index}: {:.4} s for a {seconds} s note ({measured:.4?})",
                measured[index]
            );
        }
        for (index, seconds) in [0.5f64, 1.0, 2.0, 0.5].iter().enumerate() {
            assert!(
                measured[index] >= seconds * 0.99,
                "note {index}: {:.4} s is shorter than the {seconds} s note it came from ({measured:.4?})",
                measured[index]
            );
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_90_bpm_grid_keeps_quarter_notes_and_moves_the_bar_at_beat_four() {
        let path = temp_wav("90");
        let beat_s = 60.0 / 90.0;
        write_phrase(
            &path,
            &[
                (0.0, beat_s, 130.81),
                (beat_s, beat_s, 164.81),
                (2.0 * beat_s, beat_s, 196.0),
                (3.0 * beat_s, beat_s, 261.63),
                (4.0 * beat_s, beat_s, 130.81),
            ],
        );
        let report = report(&path, grid(90.0, 4, 4), false);
        assert_eq!(report.tempo_bpm, 90.0);
        assert_eq!(report.beats_per_bar, 4.0);
        assert_near(report.beat_seconds, beat_s, 1e-12, "beat seconds");
        assert_eq!(report.notes.len(), 5, "{report:?}");
        for (index, (start, bar)) in [(0.0, 1u32), (1.0, 1), (2.0, 1), (3.0, 1), (4.0, 2)]
            .iter()
            .enumerate()
        {
            let note = &report.notes[index];
            assert_near(
                note.start_beat,
                *start,
                BOUNDARY_TOLERANCE_S / beat_s,
                &format!("note {index} start beat"),
            );
            assert_near(
                note.duration_beats,
                1.0,
                BOUNDARY_TOLERANCE_S / beat_s,
                &format!("note {index} duration"),
            );
            assert_eq!(note.bar, *bar, "note {index} bar");
            assert_eq!(note.suggested_duration, "q", "note {index} token");
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_three_four_grid_reports_three_beats_per_bar_and_dotted_half_tokens() {
        let path = temp_wav("3-4");
        write_phrase(
            &path,
            &[
                (0.0, 1.5, 98.0),   // G2, 3 beats
                (1.5, 1.5, 146.83), // D3, 3 beats
                (3.0, 1.5, 130.81), // C3, 3 beats
            ],
        );
        let report = report(&path, grid(120.0, 3, 4), false);
        assert_eq!(report.time_signature, "3/4");
        assert_eq!(report.beats_per_bar, 3.0);
        assert_eq!(report.notes.len(), 3, "{report:?}");
        for (index, start) in [0.0, 3.0, 6.0].iter().enumerate() {
            let note = &report.notes[index];
            assert_near(
                note.start_beat,
                *start,
                BOUNDARY_TOLERANCE_S / report.beat_seconds,
                &format!("note {index} start beat"),
            );
            assert_near(
                note.duration_beats,
                3.0,
                BOUNDARY_TOLERANCE_S / report.beat_seconds,
                &format!("note {index} duration"),
            );
            assert_eq!(note.bar, index as u32 + 1, "note {index} bar");
            assert_eq!(note.suggested_duration, "h.", "note {index} token");
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_off_grid_onset_is_reported_as_a_guess() {
        let path = temp_wav("off-grid");
        // Beat 0.375 is exactly between two 1/16 lines.
        write_phrase(&path, &[(0.375 * 0.5, 0.5, 220.0)]);
        let report = report(&path, grid(120.0, 4, 4), false);
        assert_eq!(report.notes.len(), 1, "{report:?}");
        let note = &report.notes[0];
        assert_near(
            note.start_beat,
            0.375,
            BOUNDARY_TOLERANCE_S / report.beat_seconds,
            "off-grid start beat",
        );
        assert_eq!(note.suggested_duration, "q");
        let warnings = report.warnings.join(" | ");
        assert!(
            warnings.contains("off the 1/16 grid"),
            "the warning has to name the grid: {warnings}"
        );
        assert!(warnings.contains("array indices 0"), "{warnings}");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_default_path_keeps_raw_beats_and_quantize_reports_both() {
        let path = temp_wav("quantize");
        // 0.55 and 0.95 beats: neither is on the 1/16 grid.
        write_phrase(&path, &[(0.55 * 0.5, 0.95 * 0.5, 220.0)]);
        let raw = report(&path, grid(120.0, 4, 4), false);
        let note = &raw.notes[0];
        assert_near(
            note.start_beat,
            0.55,
            BOUNDARY_TOLERANCE_S / raw.beat_seconds,
            "raw start beat",
        );
        assert_near(
            note.duration_beats,
            0.95,
            BOUNDARY_TOLERANCE_S / raw.beat_seconds,
            "raw duration",
        );
        assert_eq!(note.start_beat_quantised, None);
        assert_eq!(note.duration_beats_quantised, None);

        let snapped = report(&path, grid(120.0, 4, 4), true);
        let note = &snapped.notes[0];
        assert_eq!(note.start_beat_quantised, Some(0.5));
        assert_eq!(note.duration_beats_quantised, Some(1.0));
        assert_near(
            note.start_beat,
            0.55,
            BOUNDARY_TOLERANCE_S / snapped.beat_seconds,
            "raw start beat survives quantize",
        );
        assert_near(
            note.duration_beats,
            0.95,
            BOUNDARY_TOLERANCE_S / snapped.beat_seconds,
            "raw duration survives quantize",
        );
        assert_eq!(note.suggested_duration, "q");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn every_suggested_token_is_a_duration_the_dsl_parses() {
        for (token, beats) in DURATION_TOKENS {
            let source = format!(
                "@project \"token\"\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  \
                 @phrase p [1:1:0 - 4:1:0]\n    C4 {token} mf\n"
            );
            let project = oc_dsl::parse(&source)
                .unwrap_or_else(|error| panic!("{token} is not a DSL duration: {error}"));
            let parsed = project.tracks[0].phrases[0].events[0].duration.to_beats();
            assert!(
                (parsed - beats).abs() < 1e-9,
                "{token} parses to {parsed} beats, the table says {beats}"
            );
        }
    }

    #[test]
    fn duration_tokens_prefer_the_simple_form_on_a_tie() {
        assert_eq!(duration_token_for(1.0), ("q", 1.0));
        assert_eq!(duration_token_for(0.5), ("8th", 0.5));
        assert_eq!(duration_token_for(1.5), ("q.", 1.5));
        assert_eq!(duration_token_for(0.75), ("8th.", 0.75));
        assert_eq!(duration_token_for(3.0), ("h.", 3.0));
        assert_eq!(duration_token_for(2.0 / 3.0), ("qt", 2.0 / 3.0));
        assert_eq!(duration_token_for(4.0 / 3.0), ("ht", 4.0 / 3.0));
        assert_eq!(duration_token_for(6.0), ("w.", 6.0));
    }
}
