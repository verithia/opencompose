//! `gliss` expansion: the mark becomes primitives the engine already renders.
//!
//! A slide is the existing note-relative `pitch_curve`, which the interpreter
//! turns into per-frame native pitch events; a run is a series of short notes
//! at a stated subdivision, so it also plays on a sampled source.

use std::borrow::Cow;

use oc_dsl::ast::{
    Articulation, KeySignature, NoteEvent, ParamValue, Pitch, PresetDef, Track,
    performance_gate_quarters,
};
use oc_dsl::gliss::{self, Gliss, RunStyle};
use oc_dsl::timing::ProjectClock;

use super::params::*;
use crate::render_control::InstrumentProfile;

/// A note-relative curve spans at most two octaves; the same bound as the
/// primitive it lowers to.
const MAX_CURVE_CENTS: f64 = oc_synth::note_pitch_curve::MAX_SEMITONES * 100.0;

/// What the track's notes play through, when that source cannot hold a shape
/// over the pitch.
pub(super) struct FixedPitch {
    /// The preset or instrument the refusal names.
    pub name: String,
    /// A sample bank: every note's pitch comes from a recording.
    pub sampled: bool,
}

/// Everything an expansion reads that is not in the note itself.
pub(super) struct GlissContext<'a> {
    pub track: &'a str,
    /// `None` when the source is the built-in OpenSynth voice, which takes
    /// per-frame pitch events.
    pub fixed_pitch: Option<FixedPitch>,
    pub clock: &'a ProjectClock,
    pub key: Option<KeySignature>,
    pub profile: &'a InstrumentProfile,
}

/// Which source a track renders through, as far as a pitch curve is concerned.
pub(super) fn fixed_pitch_source(track: &Track, presets: &[PresetDef]) -> Option<FixedPitch> {
    let Some(preset) = track
        .preset
        .as_deref()
        .and_then(|name| presets.iter().find(|preset| preset.name == name))
    else {
        return Some(FixedPitch {
            name: format!("{}/{}", track.instrument.category, track.instrument.name),
            sampled: true,
        });
    };
    let plugin = preset.plugin.as_deref();
    if plugin == Some("builtin.open_synth") {
        return None;
    }
    let builtin = plugin.is_some_and(|id| id.starts_with("builtin."));
    let source = preset.source.as_deref();
    let sampled = source == Some("soundfont")
        || source == Some("sample")
        || preset
            .file
            .as_deref()
            .is_some_and(|file| !file.trim().is_empty())
        || (source.is_none() && !builtin);
    Some(FixedPitch {
        name: preset.name.clone(),
        sampled,
    })
}

/// Rewrite a phrase's notes: a slide gains a `pitch_curve`, a run becomes short
/// notes. A phrase with no mark is borrowed, so a project that does not use the
/// mark pays one scan per phrase and no allocation.
pub(super) fn expand<'a>(
    events: &'a [NoteEvent],
    context: &GlissContext<'_>,
) -> Result<Cow<'a, [NoteEvent]>, String> {
    let mut expanded: Vec<NoteEvent> = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let Some(mark) = gliss::form(&event.modifiers)? else {
            if !expanded.is_empty() {
                expanded.push(event.clone());
            }
            continue;
        };
        // The first mark switches the whole phrase to owned notes.
        if expanded.is_empty() {
            expanded.extend_from_slice(&events[..index]);
        }
        let destination = match mark {
            Gliss::To(pitch) => pitch,
            _ => match gliss::successor(events, index) {
                gliss::Successor::One(pitch) => pitch,
                gliss::Successor::None => {
                    return Err(format!(
                        "{} at @{} in track '{}': no note follows it, so there is nothing to slide to — write the destination pitch with gliss=G4",
                        mark.spelling(),
                        beat_text(event.start_beat),
                        context.track
                    ));
                }
                gliss::Successor::Chord(pitches) => {
                    return Err(format!(
                        "{} at @{} in track '{}': the next note is a chord ({}), so the destination is not one pitch — write it with gliss=G4",
                        mark.spelling(),
                        beat_text(event.start_beat),
                        chord_text(&pitches),
                        context.track
                    ));
                }
            },
        };
        match mark {
            Gliss::Run(style) => expanded.extend(run(event, destination, style, context)?),
            _ => expanded.push(slide(event, mark, destination, context)?),
        }
    }
    Ok(if expanded.is_empty() {
        Cow::Borrowed(events)
    } else {
        Cow::Owned(expanded)
    })
}

/// A slide is one `pitch_curve` over the note's gate: zero cents at the onset,
/// the interval to the destination at the note-off. A written length arrives
/// early and holds the destination for the rest of the note.
fn slide(
    event: &NoteEvent,
    mark: Gliss,
    destination: Pitch,
    context: &GlissContext<'_>,
) -> Result<NoteEvent, String> {
    if let Some(fixed) = &context.fixed_pitch {
        return Err(fixed_pitch_refusal(event, mark, fixed, context.track));
    }
    let gate = gate_beats(event, context)?;
    let cents = interval_cents(event.pitch, destination);
    if cents.abs() > MAX_CURVE_CENTS {
        return Err(format!(
            "{} at @{} in track '{}': {} to {} is more than two octaves, which a slide cannot span — write gliss=run, or slide in two steps",
            mark.spelling(),
            beat_text(event.start_beat),
            event.pitch.to_text(),
            destination.to_text(),
            context.track
        ));
    }
    let points = match mark {
        Gliss::Portamento(seconds) => {
            let tempo = context.clock.tempo_at_quarter(event.start_beat)?;
            let length = (seconds * tempo / 60.0).min(gate);
            if length >= gate {
                vec![(0.0, 0.0), (gate, cents)]
            } else {
                vec![(0.0, 0.0), (length, cents), (gate, cents)]
            }
        }
        _ => vec![(0.0, 0.0), (gate, cents)],
    };
    let mut slid = event.clone();
    slid.modifiers.retain(|(key, _)| key != "pitch_curve");
    slid.modifiers.push((
        "pitch_curve".to_string(),
        ParamValue::String(curve_text(&points)),
    ));
    Ok(slid)
}

/// A run keeps the note's own dynamics and splits its gate into equal steps
/// from the written pitch to the destination. Without an articulation of its
/// own each step is legato, so the instrument joins them into one gesture
/// instead of a row of attacks.
fn run(
    event: &NoteEvent,
    destination: Pitch,
    style: RunStyle,
    context: &GlissContext<'_>,
) -> Result<Vec<NoteEvent>, String> {
    let source = event.pitch.to_midi();
    let target = destination.to_midi();
    if source == target {
        return Err(format!(
            "gliss=run at @{} in track '{}': the destination is the note's own pitch {}, so there is nothing to run — name a different pitch or drop the mark",
            beat_text(event.start_beat),
            context.track,
            destination.to_text()
        ));
    }
    let pitches = match style {
        RunStyle::Chromatic => chromatic_run(source, target),
        RunStyle::Diatonic => match context.key {
            Some(key) => diatonic_run(source, target, key),
            None => {
                return Err(format!(
                    "gliss=run_diatonic at @{} in track '{}': the project declares no key — add `key: C major` to the header, or write gliss=run for a chromatic run",
                    beat_text(event.start_beat),
                    context.track
                ));
            }
        },
    };
    let step = gate_beats(event, context)? / pitches.len() as f64;
    Ok(pitches
        .into_iter()
        .enumerate()
        .map(|(index, pitch)| {
            let mut note = event.clone();
            note.pitch = pitch;
            note.start_beat = event.start_beat + step * index as f64;
            note.articulation = event.articulation.or(Some(Articulation::Legato));
            note.modifiers.retain(|(key, _)| {
                key != gliss::MODIFIER && key != "gate_quarters" && key != "pitch_curve"
            });
            note.modifiers
                .push(("gate_quarters".to_string(), ParamValue::Number(step)));
            note
        })
        .collect())
}

/// Every semitone between the two pitches, both ends included.
fn chromatic_run(source: u8, target: u8) -> Vec<Pitch> {
    let step = if target > source { 1.0 } else { -1.0 };
    let mut pitches = Vec::new();
    let mut current = source as f64;
    while (step > 0.0 && current <= target as f64) || (step < 0.0 && current >= target as f64) {
        pitches.push(midi_pitch(current as u8));
        current += step;
    }
    pitches
}

/// The project key's own pitches between the two, with the written destination
/// last: a destination outside the key still arrives.
fn diatonic_run(source: u8, target: u8, key: KeySignature) -> Vec<Pitch> {
    let scale = scale_pitch_classes(key);
    let up = target > source;
    let mut pitches = vec![midi_pitch(source)];
    let mut current = source as i32;
    while let Some(next) = next_scale_pitch(current, &scale, up) {
        if (up && next >= target as i32) || (!up && next <= target as i32) {
            break;
        }
        pitches.push(midi_pitch(next as u8));
        current = next;
    }
    pitches.push(midi_pitch(target));
    pitches
}

pub(super) fn scale_pitch_classes(key: KeySignature) -> Vec<i32> {
    let tonic = Pitch {
        note: key.root,
        accidental: key.accidental,
        octave: 4,
    }
    .to_midi() as i32;
    let intervals: &[i32] = match key.mode {
        oc_dsl::ast::KeyMode::Major => &[0, 2, 4, 5, 7, 9, 11],
        oc_dsl::ast::KeyMode::Minor => &[0, 2, 3, 5, 7, 8, 10],
    };
    intervals
        .iter()
        .map(|offset| (tonic + offset).rem_euclid(12))
        .collect()
}

fn next_scale_pitch(from: i32, scale: &[i32], up: bool) -> Option<i32> {
    let step = if up { 1 } else { -1 };
    (1..=12)
        .map(|offset| from + offset * step)
        .find(|pitch| scale.contains(&(*pitch).rem_euclid(12)))
}

fn interval_cents(from: Pitch, to: Pitch) -> f64 {
    (to.to_midi() as f64 - from.to_midi() as f64) * 100.0
}

fn midi_pitch(midi: u8) -> Pitch {
    let names = [
        (0, "C"),
        (1, "C#"),
        (2, "D"),
        (3, "D#"),
        (4, "E"),
        (5, "F"),
        (6, "F#"),
        (7, "G"),
        (8, "G#"),
        (9, "A"),
        (10, "A#"),
        (11, "B"),
    ];
    let (_, name) = names[midi as usize % 12];
    Pitch::from_text(&format!("{name}{}", midi as i32 / 12 - 1))
        .expect("a canonical pitch name always parses")
}

fn gate_beats(event: &NoteEvent, context: &GlissContext<'_>) -> Result<f64, String> {
    Ok(performance_gate_quarters(&event.modifiers)?
        .unwrap_or_else(|| note_duration_for_profile(event, context.profile)))
}

fn fixed_pitch_refusal(event: &NoteEvent, mark: Gliss, fixed: &FixedPitch, track: &str) -> String {
    let reason = if fixed.sampled {
        format!(
            "'{}' is a sampled source, so its pitch comes from the recording and cannot slide",
            fixed.name
        )
    } else {
        format!(
            "'{}' is a hosted instrument, so the note's pitch curve cannot reach it",
            fixed.name
        )
    };
    format!(
        "{} at @{} in track '{}': {reason} — use gliss=run for a run of short notes, or a builtin/OpenSynth preset (source: builtin, plugin: \"builtin.open_synth\"). read_skill('writing-notes') teaches both forms",
        mark.spelling(),
        beat_text(event.start_beat),
        track
    )
}

fn beat_text(beat: f64) -> String {
    number_text(beat)
}

fn chord_text(pitches: &[Pitch]) -> String {
    pitches
        .iter()
        .map(Pitch::to_text)
        .collect::<Vec<_>>()
        .join(" ")
}

/// `quarters:cents` list the primitive reads back.
fn curve_text(points: &[(f64, f64)]) -> String {
    points
        .iter()
        .map(|(quarters, cents)| format!("{}:{}", number_text(*quarters), number_text(*cents)))
        .collect::<Vec<_>>()
        .join(",")
}

fn number_text(value: f64) -> String {
    let mut text = format!("{value:.6}");
    if text.contains('.') {
        text = text.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if text == "-0" { "0".to_string() } else { text }
}
