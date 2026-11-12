//! Note and pattern editing on the timeline.

use tauri::State;

use oc_dsl::ast::{self, *};
use oc_dsl::timing::ProjectClock;
use serde::Deserialize;

use crate::state::AppState;
use crate::state::{UndoEdit, UndoScope};

use super::arrangement::{retimed_range, unique_block_name};
use super::shared::{
    ProjectSnapshot, commit_validated_edit, parse_accidental, parse_base_duration, parse_dynamics,
    parse_note_name, resync_with_undo, snapshot,
};

// ============================================================================
// Note editing
// ============================================================================

pub(crate) fn editor_duration(beats: f64) -> Result<Duration, String> {
    if !beats.is_finite() || beats <= 0.0 || beats > 1024.0 {
        return Err("Note duration must be finite and within (0, 1024] quarters".into());
    }
    let ticks = (beats * 48.0).round() as u32;
    if ticks == 0 || (ticks as f64 / 48.0 - beats).abs() > 1e-7 {
        return Err("Duration must align to the 1/48-quarter editor grid".into());
    }
    let remainder = ticks % 48;
    let mut quarters = ticks / 48;
    let tuplet = if remainder == 0 {
        quarters -= 1;
        None
    } else {
        let mut a = 48;
        let mut b = remainder;
        while b != 0 {
            let r = a % b;
            a = b;
            b = r;
        }
        Some(((48 / a) as u8, (remainder / a) as u8))
    };
    let mut ties = vec![ast::BaseDuration::Whole; (quarters / 4) as usize];
    quarters %= 4;
    if quarters >= 2 {
        ties.push(ast::BaseDuration::Half);
        quarters -= 2;
    }
    if quarters == 1 {
        ties.push(ast::BaseDuration::Quarter);
    }
    Ok(Duration {
        base: ast::BaseDuration::Quarter,
        dots: 0,
        triplet: false,
        tuplet,
        ties,
    })
}

/// Refuse content placed at or past a pattern's own end. The roll clamps a
/// gesture before it gets here; this is the backstop for a direct command.
pub(crate) fn refuse_outside_pattern(pattern: &ast::PatternDef, start: f64) -> Result<(), String> {
    let end = pattern.length_quarters();
    if start >= end - 1e-9 {
        return Err(format!(
            "outside pattern '{}': it ends at {} quarters ({} bars), so {start} is past the boundary",
            pattern.name,
            end,
            end / 4.0
        ));
    }
    Ok(())
}

/// The note a single add writes.
///
/// A copied payload keeps every field the AST stores and takes only the
/// command's start; without one, `fresh` describes a new note from the command's
/// own arguments.
pub(crate) fn placed_note(
    source: Option<NoteEvent>,
    start_beat: f64,
    explicit_position: bool,
    fresh: impl FnOnce() -> Result<NoteEvent, String>,
) -> Result<NoteEvent, String> {
    match source {
        Some(mut event) => {
            event.start_beat = start_beat;
            event.explicit_position = explicit_position;
            Ok(event)
        }
        None => fresh(),
    }
}

#[tauri::command]
// Tauri command arity is the frontend IPC contract.
#[allow(clippy::too_many_arguments)]
/// Insert one note into a track phrase.
pub fn add_note(
    track_index: usize,
    phrase_index: usize,
    note_name: String,
    accidental: String,
    octave: i8,
    base_duration: String,
    dynamics: Option<String>,
    start_beat: Option<f64>,
    duration_beats: Option<f64>,
    note: Option<NoteEvent>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    // A phrase is a placement too: a start at or past its end has no sound.
    if let Some(start) = start_beat
        && let Some(range) = inner
            .project
            .tracks
            .get(track_index)
            .and_then(|track| track.phrases.get(phrase_index))
            .and_then(|phrase| phrase.time_range)
    {
        let clock = ProjectClock::new(&inner.project.properties)?;
        let span = clock.quarter_notes_at(&range.end)? - clock.quarter_notes_at(&range.start)?;
        if start >= span - 1e-9 {
            return Err(format!(
                "outside phrase {phrase_index}: it ends at {span} quarters, so {start} is past the boundary"
            ));
        }
    }
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    let phrase = track
        .phrases
        .get_mut(phrase_index)
        .ok_or_else(|| format!("Phrase {} out of range", phrase_index))?;

    // Determine start_beat: use explicit value or place after last note
    let beat = start_beat.unwrap_or_else(|| {
        phrase
            .events
            .last()
            .map(|e| e.start_beat + e.duration.to_beats())
            .unwrap_or(0.0)
    });

    let event = placed_note(note, beat, start_beat.is_some(), || {
        Ok(NoteEvent {
            start_beat: beat,
            explicit_position: start_beat.is_some(),
            pitch: Pitch {
                note: parse_note_name(&note_name)?,
                accidental: parse_accidental(&accidental)?,
                octave,
            },
            duration: if let Some(beats) = duration_beats {
                editor_duration(beats)?
            } else {
                Duration {
                    base: parse_base_duration(&base_duration)?,
                    dots: 0,
                    triplet: false,
                    tuplet: None,
                    ties: vec![],
                }
            },
            dynamics: dynamics.as_deref().map(parse_dynamics).transpose()?,
            velocity: None,
            articulation: None,
            vibrato: None,
            expression: None,
            pitch_bend: None,
            muted: false,
            grace: false,
            text: None,
            modifiers: vec![],
        })
    })?;

    // Insert sorted by start_beat (allows overlapping notes / chords)
    let insert_pos = phrase
        .events
        .iter()
        .position(|e| e.start_beat > beat)
        .unwrap_or(phrase.events.len());
    phrase.events.insert(insert_pos, event);

    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("note add", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Extend a phrase range to cover its content.
#[tauri::command]
pub fn extend_phrase_range(
    track_index: usize,
    phrase_index: usize,
    end_bar: u32,
    end_beat: u32,
    end_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    let phrase = track
        .phrases
        .get_mut(phrase_index)
        .ok_or_else(|| format!("Phrase {} out of range", phrase_index))?;
    if let Some(ref mut tr) = phrase.time_range {
        tr.end = ast::BarBeat {
            bar: end_bar,
            beat: end_beat,
            subdivision: end_sub,
        };
    }
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("phrase range", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Add an empty phrase to a track.
#[tauri::command]
pub fn add_phrase(
    track_index: usize,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    let phrase_name = format!("phrase_{}", track.phrases.len() + 1);
    let phrase = ast::Phrase {
        name: phrase_name,
        time_range: Some(ast::TimeRange {
            start: ast::BarBeat {
                bar: start_bar,
                beat: start_beat,
                subdivision: start_sub,
            },
            end: ast::BarBeat {
                bar: start_bar + 2,
                beat: start_beat,
                subdivision: start_sub,
            },
        }),
        events: vec![],
        pedals: vec![],
        hairpins: Vec::new(),
        channel: None,
    };
    track.phrases.push(phrase);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("phrase add", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Paste a copied phrase at a new start, keeping its notes intact. One undo step.
#[tauri::command]
pub fn insert_phrase(
    track_index: usize,
    mut phrase: ast::Phrase,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let clock = ProjectClock::new(&inner.project.properties)?;
    let range = phrase
        .time_range
        .ok_or_else(|| "Phrase has no time range".to_string())?;
    phrase.time_range = Some(retimed_range(
        &clock,
        &range,
        ast::BarBeat {
            bar: start_bar,
            beat: start_beat,
            subdivision: start_sub,
        },
    )?);
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    phrase.name = unique_block_name(&phrase.name, track.phrases.iter().map(|p| p.name.as_str()));
    track.phrases.push(phrase);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("phrase insert", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Paste a copied inline drum pattern at a new start. One undo step.
#[tauri::command]
pub fn insert_inline_pattern(
    track_index: usize,
    mut pattern: ast::Pattern,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let clock = ProjectClock::new(&inner.project.properties)?;
    let range = pattern
        .time_range
        .ok_or_else(|| format!("Pattern {} has no time range", pattern.name))?;
    pattern.time_range = Some(retimed_range(
        &clock,
        &range,
        ast::BarBeat {
            bar: start_bar,
            beat: start_beat,
            subdivision: start_sub,
        },
    )?);
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    pattern.name = unique_block_name(
        &pattern.name,
        track.patterns.iter().map(|p| p.name.as_str()),
    );
    track.patterns.push(pattern);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("pattern insert", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Remove one note from a track phrase.
#[tauri::command]
pub fn remove_note(
    track_index: usize,
    phrase_index: usize,
    note_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    let phrase = track
        .phrases
        .get_mut(phrase_index)
        .ok_or_else(|| format!("Phrase {} out of range", phrase_index))?;
    if note_index >= phrase.events.len() {
        return Err(format!("Note {} out of range", note_index));
    }
    phrase.events.remove(note_index);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("note remove", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

// ============================================================================
// Batch note editing
// ============================================================================
// A gesture writes one command for the whole selection: every target is
// resolved against the list it names before the first write, the targets come
// from that same list, and the list is sorted once. One command is one undo
// entry.

/// One note a batch move retargets. `new_pitch` null keeps the pitch.
#[derive(Debug, Clone, Deserialize)]
pub struct NoteMove {
    #[serde(default)]
    pub phrase_index: usize,
    pub note_index: usize,
    pub new_start_beat: f64,
    #[serde(default)]
    pub new_pitch: Option<Pitch>,
}

/// One note a batch velocity write retargets.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct NoteVelocity {
    #[serde(default)]
    pub phrase_index: usize,
    pub note_index: usize,
    pub velocity: u8,
}

/// Refuse one move target the list cannot hold.
fn check_move(note_count: usize, target: &NoteMove) -> Result<(), String> {
    if target.note_index >= note_count {
        return Err(format!("Note {} out of range", target.note_index));
    }
    if !target.new_start_beat.is_finite() {
        return Err("Non-finite note position".into());
    }
    Ok(())
}

fn apply_move(note: &mut NoteEvent, target: &NoteMove) {
    if let Some(pitch) = target.new_pitch {
        note.pitch = pitch;
    }
    note.start_beat = target.new_start_beat.max(0.0);
    note.explicit_position = true;
}

fn apply_velocity(note: &mut NoteEvent, target: &NoteVelocity) -> bool {
    if note.velocity == Some(target.velocity) {
        return false;
    }
    note.velocity = Some(target.velocity);
    true
}

/// Move every selected note by its own target; one undo entry per gesture.
#[tauri::command]
pub fn move_notes(
    scope: NoteScope,
    moves: Vec<NoteMove>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    move_notes_at(&state, scope, &moves)
}

pub(crate) fn move_notes_at(
    state: &AppState,
    scope: NoteScope,
    moves: &[NoteMove],
) -> Result<ProjectSnapshot, String> {
    move_notes_labelled_at(state, scope, moves, "note move")
}

pub(crate) fn move_notes_labelled_at(
    state: &AppState,
    scope: NoteScope,
    moves: &[NoteMove],
    label: &'static str,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let mut changed = false;
    match &scope {
        NoteScope::Track { track_index } => {
            let track = inner
                .project
                .tracks
                .get_mut(*track_index)
                .ok_or_else(|| format!("Track {} out of range", track_index))?;
            for target in moves {
                let phrase = track
                    .phrases
                    .get(target.phrase_index)
                    .ok_or_else(|| format!("Phrase {} out of range", target.phrase_index))?;
                check_move(phrase.events.len(), target)?;
            }
            for target in moves {
                let note = &mut track.phrases[target.phrase_index].events[target.note_index];
                apply_move(note, target);
                changed = true;
            }
            let mut touched: Vec<usize> = moves.iter().map(|target| target.phrase_index).collect();
            touched.sort_unstable();
            touched.dedup();
            for phrase_index in touched {
                sort_notes(&mut track.phrases[phrase_index].events);
            }
        }
        NoteScope::Pattern { pattern_name } => {
            let pattern = inner
                .project
                .patterns
                .iter_mut()
                .find(|pattern| pattern.name == *pattern_name)
                .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
            for target in moves {
                check_move(pattern.events.len(), target)?;
            }
            for target in moves {
                let note = &mut pattern.events[target.note_index];
                apply_move(note, target);
                changed = true;
            }
            if changed {
                sort_notes(&mut pattern.events);
            }
        }
    }
    if changed {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new(label, UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

/// Remove every selected note; one undo entry restores all of them.
#[tauri::command]
pub fn remove_notes(
    scope: NoteScope,
    notes: Vec<NoteRef>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    remove_notes_at(&state, scope, &notes)
}

pub(crate) fn remove_notes_at(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteRef],
) -> Result<ProjectSnapshot, String> {
    remove_notes_labelled_at(state, scope, notes, "note remove")
}

pub(crate) fn remove_notes_labelled_at(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteRef],
    label: &'static str,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let mut changed = false;
    match &scope {
        NoteScope::Track { track_index } => {
            let track = inner
                .project
                .tracks
                .get_mut(*track_index)
                .ok_or_else(|| format!("Track {} out of range", track_index))?;
            check_phrase_refs(track.phrases.len(), notes)?;
            let mut by_phrase: Vec<Vec<usize>> = vec![Vec::new(); track.phrases.len()];
            for note in notes {
                by_phrase[note.phrase_index].push(note.note_index);
            }
            for (phrase_index, indices) in by_phrase.iter_mut().enumerate() {
                check_note_refs(track.phrases[phrase_index].events.len(), indices)?;
                indices.sort_unstable();
                indices.dedup();
            }
            for (phrase_index, indices) in by_phrase.into_iter().enumerate() {
                for index in indices.into_iter().rev() {
                    track.phrases[phrase_index].events.remove(index);
                    changed = true;
                }
            }
        }
        NoteScope::Pattern { pattern_name } => {
            let pattern = inner
                .project
                .patterns
                .iter_mut()
                .find(|pattern| pattern.name == *pattern_name)
                .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
            let indices = note_indices(notes, None);
            check_note_refs(pattern.events.len(), &indices)?;
            for index in indices.into_iter().rev() {
                pattern.events.remove(index);
                changed = true;
            }
        }
    }
    if changed {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new(label, UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

/// One note a batch paste writes, and the phrase it lands in.
///
/// The note is the whole [`NoteEvent`] the clipboard copied, so every field the
/// AST stores survives the trip; only `start_beat` is the editor's target.
#[derive(Debug, Clone, Deserialize)]
pub struct NoteInsert {
    #[serde(default)]
    pub phrase_index: usize,
    pub note: NoteEvent,
}

/// Refuse a paste target the scope cannot hold.
fn check_insert(insert: &NoteInsert) -> Result<(), String> {
    if !insert.note.start_beat.is_finite() {
        return Err("Non-finite note position".into());
    }
    Ok(())
}

/// The note a paste writes: its own fields, at the target start.
fn inserted_note(insert: &NoteInsert) -> NoteEvent {
    let mut event = insert.note.clone();
    event.start_beat = event.start_beat.max(0.0);
    event.explicit_position = true;
    event
}

/// Paste every copied note into one container; one undo entry per paste.
#[tauri::command]
pub fn add_notes(
    scope: NoteScope,
    notes: Vec<NoteInsert>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    add_notes_at(&state, scope, &notes)
}

pub(crate) fn add_notes_at(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteInsert],
) -> Result<ProjectSnapshot, String> {
    add_notes_labelled_at(state, scope, notes, "note paste")
}

pub(crate) fn add_notes_labelled_at(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteInsert],
    label: &'static str,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    match &scope {
        NoteScope::Track { track_index } => {
            let track = inner
                .project
                .tracks
                .get_mut(*track_index)
                .ok_or_else(|| format!("Track {} out of range", track_index))?;
            for insert in notes {
                if insert.phrase_index >= track.phrases.len() {
                    return Err(format!("Phrase {} out of range", insert.phrase_index));
                }
                check_insert(insert)?;
            }
            let mut touched: Vec<usize> = Vec::new();
            for insert in notes {
                track.phrases[insert.phrase_index]
                    .events
                    .push(inserted_note(insert));
                touched.push(insert.phrase_index);
            }
            touched.sort_unstable();
            touched.dedup();
            for phrase_index in touched {
                sort_notes(&mut track.phrases[phrase_index].events);
            }
        }
        NoteScope::Pattern { pattern_name } => {
            let pattern = inner
                .project
                .patterns
                .iter_mut()
                .find(|pattern| pattern.name == *pattern_name)
                .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
            for insert in notes {
                check_insert(insert)?;
                refuse_outside_pattern(pattern, insert.note.start_beat)?;
            }
            for insert in notes {
                pattern.events.push(inserted_note(insert));
            }
            if !notes.is_empty() {
                sort_notes(&mut pattern.events);
            }
        }
    }
    if !notes.is_empty() {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new(label, UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

/// Duplicate every selected note by the same offset; one undo entry.
#[tauri::command]
pub fn duplicate_notes(
    scope: NoteScope,
    notes: Vec<NoteRef>,
    offset_beats: f64,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    duplicate_notes_at(&state, scope, &notes, offset_beats)
}

pub(crate) fn duplicate_notes_at(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteRef],
    offset_beats: f64,
) -> Result<ProjectSnapshot, String> {
    if !offset_beats.is_finite() {
        return Err("Non-finite duplicate offset".into());
    }
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    match &scope {
        NoteScope::Track { track_index } => {
            let track = inner
                .project
                .tracks
                .get_mut(*track_index)
                .ok_or_else(|| format!("Track {} out of range", track_index))?;
            let mut by_phrase: Vec<Vec<usize>> = vec![Vec::new(); track.phrases.len()];
            for note in notes {
                let indices = by_phrase
                    .get_mut(note.phrase_index)
                    .ok_or_else(|| format!("Phrase {} out of range", note.phrase_index))?;
                indices.push(note.note_index);
            }
            for (phrase_index, indices) in by_phrase.iter_mut().enumerate() {
                check_note_refs(track.phrases[phrase_index].events.len(), indices)?;
                indices.sort_unstable();
                indices.dedup();
            }
            for (phrase_index, indices) in by_phrase.into_iter().enumerate() {
                if indices.is_empty() {
                    continue;
                }
                let events = &mut track.phrases[phrase_index].events;
                for index in indices {
                    let mut copy = events[index].clone();
                    copy.start_beat = (copy.start_beat + offset_beats).max(0.0);
                    copy.explicit_position = true;
                    events.push(copy);
                }
                sort_notes(events);
            }
        }
        NoteScope::Pattern { pattern_name } => {
            let pattern = inner
                .project
                .patterns
                .iter_mut()
                .find(|pattern| pattern.name == *pattern_name)
                .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
            let indices = note_indices(notes, None);
            check_note_refs(pattern.events.len(), &indices)?;
            for index in indices {
                let mut copy = pattern.events[index].clone();
                copy.start_beat = (copy.start_beat + offset_beats).max(0.0);
                copy.explicit_position = true;
                pattern.events.push(copy);
            }
            if !notes.is_empty() {
                sort_notes(&mut pattern.events);
            }
        }
    }
    if !notes.is_empty() {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new("note duplicate", UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

/// Write an exact velocity on every selected note; one undo entry per gesture.
#[tauri::command]
pub fn set_note_velocities(
    scope: NoteScope,
    values: Vec<NoteVelocity>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    set_note_velocities_at(&state, scope, &values)
}

pub(crate) fn set_note_velocities_at(
    state: &AppState,
    scope: NoteScope,
    values: &[NoteVelocity],
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let mut changed = false;
    match &scope {
        NoteScope::Track { track_index } => {
            let track = inner
                .project
                .tracks
                .get_mut(*track_index)
                .ok_or_else(|| format!("Track {} out of range", track_index))?;
            for target in values {
                let phrase = track
                    .phrases
                    .get(target.phrase_index)
                    .ok_or_else(|| format!("Phrase {} out of range", target.phrase_index))?;
                if target.note_index >= phrase.events.len() {
                    return Err(format!("Note {} out of range", target.note_index));
                }
            }
            for target in values {
                let note = &mut track.phrases[target.phrase_index].events[target.note_index];
                changed |= apply_velocity(note, target);
            }
        }
        NoteScope::Pattern { pattern_name } => {
            let pattern = inner
                .project
                .patterns
                .iter_mut()
                .find(|pattern| pattern.name == *pattern_name)
                .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
            if let Some(target) = values
                .iter()
                .find(|target| target.note_index >= pattern.events.len())
            {
                return Err(format!("Note {} out of range", target.note_index));
            }
            for target in values {
                let note = &mut pattern.events[target.note_index];
                changed |= apply_velocity(note, target);
            }
        }
    }
    if changed {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new("note velocity", UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

// ============================================================================
// Note editing (move, resize, properties)
// ============================================================================

/// Move or resize one note in a track phrase.
#[tauri::command]
pub fn move_note(
    track_index: usize,
    phrase_index: usize,
    note_index: usize,
    new_start_beat: f64,
    new_pitch: Option<Pitch>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    let phrase = track
        .phrases
        .get_mut(phrase_index)
        .ok_or_else(|| format!("Phrase {} out of range", phrase_index))?;
    let note = phrase
        .events
        .get_mut(note_index)
        .ok_or_else(|| format!("Note {} out of range", note_index))?;
    if !new_start_beat.is_finite() {
        return Err("Non-finite note position".into());
    }
    if let Some(pitch) = new_pitch {
        note.pitch = pitch;
    }
    note.start_beat = new_start_beat.max(0.0);
    note.explicit_position = true;
    // Re-sort events by start_beat
    phrase.events.sort_by(|a, b| {
        a.start_beat
            .partial_cmp(&b.start_beat)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("note move", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

#[tauri::command]
// Tauri command arity is the frontend IPC contract.
#[allow(clippy::too_many_arguments)]
/// Set one note duration from a base value or explicit beats.
pub fn set_note_duration(
    track_index: usize,
    phrase_index: usize,
    note_index: usize,
    base_duration: String,
    dots: u8,
    triplet: bool,
    duration_beats: Option<f64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let duration = if let Some(beats) = duration_beats {
        editor_duration(beats)?
    } else {
        Duration {
            base: parse_base_duration(&base_duration)?,
            dots,
            triplet,
            tuplet: None,
            ties: vec![],
        }
    };
    edit_note_at(
        &state,
        NoteScope::Track { track_index },
        NoteRef {
            phrase_index,
            note_index,
        },
        "note duration",
        |note| note.duration = duration,
    )
}

/// Set one note exact velocity.
#[tauri::command]
pub fn set_note_velocity(
    track_index: usize,
    phrase_index: usize,
    note_index: usize,
    velocity: u8,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    edit_note_at(
        &state,
        NoteScope::Track { track_index },
        NoteRef {
            phrase_index,
            note_index,
        },
        "note velocity",
        |note| note.velocity = Some(velocity),
    )
}

/// Drop a note's exact velocity so its written dynamic is what plays again.
///
/// A note that already follows its dynamic is left alone and records no undo
/// entry; the caller disables the control in that case.
#[tauri::command]
pub fn clear_note_velocity(
    track_index: usize,
    phrase_index: usize,
    note_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    clear_note_velocity_at(&state, track_index, phrase_index, note_index)
}

pub(crate) fn clear_note_velocity_at(
    state: &AppState,
    track_index: usize,
    phrase_index: usize,
    note_index: usize,
) -> Result<ProjectSnapshot, String> {
    edit_note_at_if_changed(
        state,
        NoteScope::Track { track_index },
        NoteRef {
            phrase_index,
            note_index,
        },
        "clear note velocity",
        |note| {
            if note.velocity.is_none() {
                return false;
            }
            note.velocity = None;
            true
        },
    )
}

/// Set or clear a note's dynamics token. Clearing an exact velocity in the same
/// edit keeps the token audible: `velocity` overrides `dynamics` everywhere.
#[tauri::command]
pub fn set_note_dynamics(
    track_index: usize,
    phrase_index: usize,
    note_index: usize,
    dynamics: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let parsed = dynamics.as_deref().map(parse_dynamics).transpose()?;
    edit_note_at(
        &state,
        NoteScope::Track { track_index },
        NoteRef {
            phrase_index,
            note_index,
        },
        "note dynamics",
        |note| {
            note.dynamics = parsed;
            if parsed.is_some() {
                note.velocity = None;
            }
        },
    )
}

/// Set or clear a note's articulation, named by its DSL token.
#[tauri::command]
pub fn set_note_articulation(
    track_index: usize,
    phrase_index: usize,
    note_index: usize,
    articulation: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let parsed = articulation
        .as_deref()
        .map(|token| {
            Articulation::from_token(token)
                .ok_or_else(|| format!("Unknown articulation: '{}'", token))
        })
        .transpose()?;
    edit_note_at(
        &state,
        NoteScope::Track { track_index },
        NoteRef {
            phrase_index,
            note_index,
        },
        "note articulation",
        |note| note.articulation = parsed,
    )
}

/// Mute or unmute one note.
#[tauri::command]
pub fn set_note_muted(
    track_index: usize,
    phrase_index: usize,
    note_index: usize,
    muted: bool,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    edit_note_at(
        &state,
        NoteScope::Track { track_index },
        NoteRef {
            phrase_index,
            note_index,
        },
        "note mute",
        |note| note.muted = muted,
    )
}

/// Mark one note as a grace note.
#[tauri::command]
pub fn set_note_grace(
    track_index: usize,
    phrase_index: usize,
    note_index: usize,
    grace: bool,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    edit_note_at(
        &state,
        NoteScope::Track { track_index },
        NoteRef {
            phrase_index,
            note_index,
        },
        "note grace",
        |note| note.grace = grace,
    )
}

/// Edit one note addressed by scope, recording undo only when `edit` reports a
/// change. Lookup and snapshot stay shared by every note command.
fn edit_note_at_if_changed(
    state: &AppState,
    scope: NoteScope,
    note: NoteRef,
    label: &'static str,
    edit: impl FnOnce(&mut NoteEvent) -> bool,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let target = match &scope {
        NoteScope::Track { track_index } => inner
            .project
            .tracks
            .get_mut(*track_index)
            .ok_or_else(|| format!("Track {} out of range", track_index))?
            .phrases
            .get_mut(note.phrase_index)
            .ok_or_else(|| format!("Phrase {} out of range", note.phrase_index))?
            .events
            .get_mut(note.note_index)
            .ok_or_else(|| format!("Note {} out of range", note.note_index))?,
        NoteScope::Pattern { pattern_name } => inner
            .project
            .patterns
            .iter_mut()
            .find(|pattern| pattern.name == *pattern_name)
            .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?
            .events
            .get_mut(note.note_index)
            .ok_or_else(|| format!("Note {} out of range", note.note_index))?,
    };
    if edit(target) {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new(label, UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

/// Edit one note that always produces history, such as a parsed field write.
fn edit_note_at(
    state: &AppState,
    scope: NoteScope,
    note: NoteRef,
    label: &'static str,
    edit: impl FnOnce(&mut NoteEvent),
) -> Result<ProjectSnapshot, String> {
    edit_note_at_if_changed(state, scope, note, label, |note| {
        edit(note);
        true
    })
}

// Pattern note editing
#[tauri::command]
// Tauri command arity is the frontend IPC contract.
#[allow(clippy::too_many_arguments)]
/// Insert one note into a named pattern.
pub fn add_pattern_note(
    pattern_name: String,
    note_name: String,
    accidental: String,
    octave: i8,
    base_duration: String,
    start_beat: f64,
    duration_beats: Option<f64>,
    note: Option<NoteEvent>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    add_pattern_note_at(
        &state,
        &pattern_name,
        &note_name,
        &accidental,
        octave,
        &base_duration,
        start_beat,
        duration_beats,
        note,
    )?;
    let inner = state.lock_inner();
    Ok(snapshot(&inner))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_pattern_note_at(
    state: &AppState,
    pattern_name: &str,
    note_name: &str,
    accidental: &str,
    octave: i8,
    base_duration: &str,
    start_beat: f64,
    duration_beats: Option<f64>,
    note: Option<NoteEvent>,
) -> Result<(), String> {
    if !start_beat.is_finite() {
        return Err("Note start must be finite".into());
    }
    let start = start_beat.max(0.0);
    let event = placed_note(note, start, true, || {
        Ok(NoteEvent {
            start_beat: start,
            explicit_position: true,
            pitch: Pitch {
                note: parse_note_name(note_name)?,
                accidental: parse_accidental(accidental)?,
                octave,
            },
            duration: if let Some(beats) = duration_beats {
                editor_duration(beats)?
            } else {
                Duration {
                    base: parse_base_duration(base_duration)?,
                    dots: 0,
                    triplet: false,
                    tuplet: None,
                    ties: vec![],
                }
            },
            dynamics: None,
            articulation: None,
            vibrato: None,
            expression: None,
            pitch_bend: None,
            velocity: None,
            muted: false,
            grace: false,
            text: None,
            modifiers: vec![],
        })
    })?;
    add_notes_labelled_at(
        state,
        NoteScope::Pattern {
            pattern_name: pattern_name.to_owned(),
        },
        &[NoteInsert {
            phrase_index: 0,
            note: event,
        }],
        "pattern note add",
    )?;
    Ok(())
}

/// Remove one note from a named pattern.
#[tauri::command]
pub fn remove_pattern_note(
    pattern_name: String,
    note_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    remove_notes_labelled_at(
        &state,
        NoteScope::Pattern { pattern_name },
        &[NoteRef {
            phrase_index: 0,
            note_index,
        }],
        "pattern note remove",
    )
}

/// Move or resize one note in a named pattern.
#[tauri::command]
pub fn move_pattern_note(
    pattern_name: String,
    note_index: usize,
    new_start_beat: f64,
    new_pitch: Option<Pitch>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    move_notes_labelled_at(
        &state,
        NoteScope::Pattern { pattern_name },
        &[NoteMove {
            phrase_index: 0,
            note_index,
            new_start_beat,
            new_pitch,
        }],
        "pattern note move",
    )
}

/// Set one pattern note exact velocity.
#[tauri::command]
pub fn set_pattern_note_velocity(
    pattern_name: String,
    note_index: usize,
    velocity: u8,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    edit_note_at(
        &state,
        NoteScope::Pattern { pattern_name },
        NoteRef {
            phrase_index: 0,
            note_index,
        },
        "pattern note velocity",
        |note| note.velocity = Some(velocity),
    )
}

/// Drop a pattern note's exact velocity so its written dynamic applies again.
#[tauri::command]
pub fn clear_pattern_note_velocity(
    pattern_name: String,
    note_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    clear_pattern_note_velocity_at(&state, &pattern_name, note_index)
}

pub(crate) fn clear_pattern_note_velocity_at(
    state: &AppState,
    pattern_name: &str,
    note_index: usize,
) -> Result<ProjectSnapshot, String> {
    edit_note_at_if_changed(
        state,
        NoteScope::Pattern {
            pattern_name: pattern_name.to_owned(),
        },
        NoteRef {
            phrase_index: 0,
            note_index,
        },
        "clear pattern note velocity",
        |note| {
            if note.velocity.is_none() {
                return false;
            }
            note.velocity = None;
            true
        },
    )
}

/// Set a pattern hit's exact velocity: the drum lane's counterpart of the note
/// command.
#[tauri::command]
pub fn set_pattern_hit_velocity(
    pattern_name: String,
    hit_index: usize,
    velocity: u8,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    set_pattern_hit_velocity_at(&state, &pattern_name, hit_index, velocity)
}

pub(crate) fn set_pattern_hit_velocity_at(
    state: &AppState,
    pattern_name: &str,
    hit_index: usize,
    velocity: u8,
) -> Result<ProjectSnapshot, String> {
    if velocity == 0 || velocity > 127 {
        return Err("Hit velocity must be between 1 and 127".into());
    }
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let pattern = inner
        .project
        .patterns
        .iter_mut()
        .find(|p| p.name == pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
    let hit = pattern
        .hits
        .get_mut(hit_index)
        .ok_or_else(|| format!("Hit {} out of range", hit_index))?;
    if hit.velocity == Some(velocity) {
        return Ok(snapshot(&inner));
    }
    hit.velocity = Some(velocity);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("pattern hit velocity", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Drop a pattern hit's exact velocity so its written dynamic applies again.
#[tauri::command]
pub fn clear_pattern_hit_velocity(
    pattern_name: String,
    hit_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    clear_pattern_hit_velocity_at(&state, &pattern_name, hit_index)
}

pub(crate) fn clear_pattern_hit_velocity_at(
    state: &AppState,
    pattern_name: &str,
    hit_index: usize,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let pattern = inner
        .project
        .patterns
        .iter_mut()
        .find(|p| p.name == pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
    let hit = pattern
        .hits
        .get_mut(hit_index)
        .ok_or_else(|| format!("Hit {} out of range", hit_index))?;
    if hit.velocity.is_none() {
        return Ok(snapshot(&inner));
    }
    hit.velocity = None;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("clear pattern hit velocity", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Set one pattern note dynamics token.
#[tauri::command]
pub fn set_pattern_note_dynamics(
    pattern_name: String,
    note_index: usize,
    dynamics: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let parsed = dynamics.as_deref().map(parse_dynamics).transpose()?;
    edit_note_at(
        &state,
        NoteScope::Pattern { pattern_name },
        NoteRef {
            phrase_index: 0,
            note_index,
        },
        "pattern note dynamics",
        |note| {
            note.dynamics = parsed;
            if parsed.is_some() {
                note.velocity = None;
            }
        },
    )
}

/// Set one pattern note articulation.
#[tauri::command]
pub fn set_pattern_note_articulation(
    pattern_name: String,
    note_index: usize,
    articulation: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let parsed = articulation
        .as_deref()
        .map(|token| {
            Articulation::from_token(token)
                .ok_or_else(|| format!("Unknown articulation: '{}'", token))
        })
        .transpose()?;
    edit_note_at(
        &state,
        NoteScope::Pattern { pattern_name },
        NoteRef {
            phrase_index: 0,
            note_index,
        },
        "pattern note articulation",
        |note| note.articulation = parsed,
    )
}

/// Mute or unmute one pattern note.
#[tauri::command]
pub fn set_pattern_note_muted(
    pattern_name: String,
    note_index: usize,
    muted: bool,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    edit_note_at(
        &state,
        NoteScope::Pattern { pattern_name },
        NoteRef {
            phrase_index: 0,
            note_index,
        },
        "pattern note mute",
        |note| note.muted = muted,
    )
}

/// Mark one pattern note as a grace note.
#[tauri::command]
pub fn set_pattern_note_grace(
    pattern_name: String,
    note_index: usize,
    grace: bool,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    edit_note_at(
        &state,
        NoteScope::Pattern { pattern_name },
        NoteRef {
            phrase_index: 0,
            note_index,
        },
        "pattern note grace",
        |note| note.grace = grace,
    )
}

/// Set one pattern note duration.
#[tauri::command]
pub fn set_pattern_note_duration(
    pattern_name: String,
    note_index: usize,
    base_duration: String,
    dots: u8,
    triplet: bool,
    duration_beats: Option<f64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let duration = if let Some(beats) = duration_beats {
        editor_duration(beats)?
    } else {
        Duration {
            base: parse_base_duration(&base_duration)?,
            dots,
            triplet,
            tuplet: None,
            ties: vec![],
        }
    };
    edit_note_at(
        &state,
        NoteScope::Pattern { pattern_name },
        NoteRef {
            phrase_index: 0,
            note_index,
        },
        "pattern note duration",
        |note| note.duration = duration,
    )
}

// ============================================================================
// Batch note tools (quantise, humanise, legato)
// ============================================================================

/// One 1/48 of a quarter: the editor's own duration grid.
pub(crate) const TICKS_PER_QUARTER: f64 = 48.0;

/// Refusal when a container is smaller than the step a tool works on.
const SHORTER_THAN_GRID: &str = "The pattern is shorter than one grid step.";

const TIMING_SALT: u64 = 0x51ED_2701_9E37_79B9;
const VELOCITY_SALT: u64 = 0x2545_F491_4F6C_DD1D;

/// Where a batch edit acts: one track's phrases, or one named pattern.
///
/// A track scope covers several phrases at once, so every note names its own
/// phrase. A pattern scope has one note list and ignores the phrase index.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NoteScope {
    Track { track_index: usize },
    Pattern { pattern_name: String },
}

/// One note inside a scope.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct NoteRef {
    #[serde(default)]
    pub phrase_index: usize,
    pub note_index: usize,
}

/// End of a note list, used as the container end when it has no authored range.
fn content_extent(events: &[NoteEvent]) -> f64 {
    events
        .iter()
        .map(|event| event.start_beat + event.duration.to_beats())
        .fold(0.0, f64::max)
}

/// Quarter-note room a note may occupy: the authored range, else the content.
fn container_span(
    range: Option<&TimeRange>,
    events: &[NoteEvent],
    clock: &ProjectClock,
) -> Result<f64, String> {
    match range {
        Some(range) => {
            Ok(clock.quarter_notes_at(&range.end)? - clock.quarter_notes_at(&range.start)?)
        }
        None => Ok(content_extent(events)),
    }
}

/// Indices inside one note list, deduplicated so a repeated ref edits once.
fn note_indices(notes: &[NoteRef], phrase_index: Option<usize>) -> Vec<usize> {
    let mut indices: Vec<usize> = notes
        .iter()
        .filter(|note| phrase_index.is_none_or(|index| note.phrase_index == index))
        .map(|note| note.note_index)
        .collect();
    indices.sort_unstable();
    indices.dedup();
    indices
}

/// Refuse a selection that names a phrase the track does not have.
fn check_phrase_refs(phrases: usize, notes: &[NoteRef]) -> Result<(), String> {
    match notes.iter().find(|note| note.phrase_index >= phrases) {
        Some(note) => Err(format!("Phrase {} out of range", note.phrase_index)),
        None => Ok(()),
    }
}

/// Refuse a selection that names a note the list does not have.
fn check_note_refs(notes: usize, selected: &[usize]) -> Result<(), String> {
    match selected.iter().find(|index| **index >= notes) {
        Some(index) => Err(format!("Note {} out of range", index)),
        None => Ok(()),
    }
}

/// One container a gesture edits: its selected notes and the room they occupy.
struct Plan {
    phrase_index: usize,
    selected: Vec<usize>,
    span: f64,
}

/// Plans for a track gesture. Every refusal happens here, before the first edit,
/// so a rejected gesture cannot leave half its notes changed without an undo entry.
fn track_plans(
    track: &ast::Track,
    notes: &[NoteRef],
    clock: &ProjectClock,
) -> Result<Vec<Plan>, String> {
    check_phrase_refs(track.phrases.len(), notes)?;
    let mut plans = Vec::new();
    for (phrase_index, phrase) in track.phrases.iter().enumerate() {
        let selected = note_indices(notes, Some(phrase_index));
        if selected.is_empty() {
            continue;
        }
        check_note_refs(phrase.events.len(), &selected)?;
        let span = container_span(phrase.time_range.as_ref(), &phrase.events, clock)?;
        plans.push(Plan {
            phrase_index,
            selected,
            span,
        });
    }
    Ok(plans)
}

/// Plan for a pattern gesture; the pattern's own content bounds it.
fn pattern_plan(events: &[NoteEvent], notes: &[NoteRef]) -> Result<Plan, String> {
    let selected = note_indices(notes, None);
    check_note_refs(events.len(), &selected)?;
    Ok(Plan {
        phrase_index: 0,
        selected,
        span: content_extent(events),
    })
}

/// Refuse when a container that holds selected notes is smaller than the step.
fn check_grid_fits(plans: &[Plan], grid: f64) -> Result<(), String> {
    if plans
        .iter()
        .any(|plan| !plan.selected.is_empty() && plan.span < grid)
    {
        return Err(SHORTER_THAN_GRID.into());
    }
    Ok(())
}

/// Keep the note list ordered by start, as `move_note` does.
fn sort_notes(events: &mut [NoteEvent]) {
    events.sort_by(|a, b| {
        a.start_beat
            .partial_cmp(&b.start_beat)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Where a start lands at a strength in 0..=100 percent: 0 keeps it, 100 snaps it.
fn quantised_start(start: f64, grid: f64, strength: f64) -> f64 {
    let snapped = (start / grid).round() * grid;
    start + (snapped - start) * (strength / 100.0)
}

/// Quantise one note list; true when a start moved.
fn quantise_list(
    events: &mut [NoteEvent],
    selected: &[usize],
    span: f64,
    grid: f64,
    strength: f64,
) -> bool {
    // A note may not start at the container end, so one editor tick is the last
    // legal start inside it.
    let last_start = (span - 1.0 / TICKS_PER_QUARTER).max(0.0);
    let mut moved = false;
    for &index in selected {
        let note = &mut events[index];
        let target = quantised_start(note.start_beat, grid, strength).clamp(0.0, last_start);
        if (target - note.start_beat).abs() > 1e-9 {
            note.start_beat = target;
            note.explicit_position = true;
            moved = true;
        }
    }
    moved
}

/// Centred offset in `-range ..= range` derived from the note's identity.
///
/// The same seed, phrase and note always give the same offset, so a humanise
/// pass is reproducible and no global random state is involved.
fn note_offset(seed: u64, phrase_index: usize, note_index: usize, salt: u64, range: i64) -> i64 {
    if range <= 0 {
        return 0;
    }
    let mut value = seed
        ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (phrase_index as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
        ^ (note_index as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 31;
    (value % (range as u64 * 2 + 1)) as i64 - range
}

/// Loudness a humanised velocity starts from: the exact velocity, else the
/// written dynamic's nominal value.
fn base_velocity(note: &NoteEvent) -> u8 {
    note.velocity
        .unwrap_or_else(|| note.dynamics.map_or(80, |dynamics| dynamics.to_velocity()))
}

/// Humanise one note list; true when a start or velocity changed.
///
/// Starts move only, so a written duration keeps its dots and tuplets. A nudge
/// never crosses the next note's start: a note with no gap to its neighbour can
/// only move earlier, because its forward room is that gap.
fn humanise_list(
    events: &mut [NoteEvent],
    selected: &[usize],
    span: f64,
    phrase_index: usize,
    timing_ticks: u32,
    velocity_spread: u8,
    seed: u64,
) -> bool {
    let starts: Vec<f64> = events.iter().map(|event| event.start_beat).collect();
    let mut changed = false;
    for &index in selected {
        let start = starts[index];
        let duration = events[index].duration.to_beats();
        let next = starts
            .iter()
            .enumerate()
            .filter(|(other, other_start)| *other != index && **other_start > start + 1e-9)
            .map(|(_, other_start)| *other_start)
            .fold(f64::INFINITY, f64::min);
        let mut limit = (span - duration).max(0.0);
        // Only a note that already clears its neighbour takes that ceiling; one
        // that overlaps it stays free so the ceiling cannot drag it backwards.
        if next.is_finite() && start + duration <= next + 1e-9 {
            limit = limit.min((next - duration).max(start));
        }
        let offset = note_offset(
            seed,
            phrase_index,
            index,
            TIMING_SALT,
            i64::from(timing_ticks),
        ) as f64
            / TICKS_PER_QUARTER;
        let target = (start + offset).clamp(0.0, limit);
        if (target - start).abs() > 1e-9 {
            events[index].start_beat = target;
            events[index].explicit_position = true;
            changed = true;
        }

        let spread = note_offset(
            seed,
            phrase_index,
            index,
            VELOCITY_SALT,
            i64::from(velocity_spread),
        );
        if spread != 0 {
            let velocity =
                (i16::from(base_velocity(&events[index])) + spread as i16).clamp(1, 127) as u8;
            if events[index].velocity != Some(velocity) {
                events[index].velocity = Some(velocity);
                changed = true;
            }
        }
    }
    changed
}

/// Fit an end onto the editor tick grid, never past `ceiling`; None when nothing fits.
fn fitted_duration(start: f64, end: f64, ceiling: f64) -> Option<f64> {
    let mut ticks = ((end - start) * TICKS_PER_QUARTER).round();
    while ticks >= 1.0 && start + ticks / TICKS_PER_QUARTER > ceiling + 1e-9 {
        ticks -= 1.0;
    }
    (ticks >= 1.0).then(|| ticks / TICKS_PER_QUARTER)
}

/// Lengthen one note list; true when a duration grew.
///
/// A note ends at its next start, or `amount` beats further on. The next start
/// and the container end bound both, so a note never overlaps and never leaves
/// its container. Without an amount, a note with no successor keeps its end.
fn legato_list(
    events: &mut [NoteEvent],
    selected: &[usize],
    span: f64,
    amount: Option<f64>,
) -> bool {
    let starts: Vec<f64> = events.iter().map(|event| event.start_beat).collect();
    let mut changed = false;
    for &index in selected {
        let start = starts[index];
        let current = events[index].duration.to_beats();
        let next = starts
            .iter()
            .enumerate()
            .filter(|(other, other_start)| *other != index && **other_start > start + 1e-9)
            .map(|(_, other_start)| *other_start)
            .fold(f64::INFINITY, f64::min);
        let ceiling = next.min(span);
        let target = match amount {
            Some(amount) => (start + amount).min(ceiling),
            // A note with no successor has nothing to reach, so it keeps its end.
            None if !next.is_finite() => continue,
            None => ceiling,
        };
        let Some(duration) = fitted_duration(start, target, ceiling) else {
            continue;
        };
        // Legato only lengthens: an overhanging note keeps its written duration.
        if duration <= current + 1e-9 {
            continue;
        }
        // The fitted value is tick-aligned, so this cannot fail; a note past the
        // 1024-quarter limit is left alone rather than failing the gesture.
        let Ok(duration) = editor_duration(duration) else {
            continue;
        };
        events[index].duration = duration;
        changed = true;
    }
    changed
}

/// Snap starts toward the grid by `strength_percent`; one undo entry per call.
#[tauri::command]
pub fn quantise_notes(
    scope: NoteScope,
    notes: Vec<NoteRef>,
    grid_quarters: f64,
    strength_percent: f64,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    quantise_notes_at(&state, scope, &notes, grid_quarters, strength_percent)
}

pub(crate) fn quantise_notes_at(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteRef],
    grid_quarters: f64,
    strength_percent: f64,
) -> Result<ProjectSnapshot, String> {
    if !grid_quarters.is_finite() || grid_quarters <= 0.0 {
        return Err("Grid must be a positive number of quarters".into());
    }
    if !strength_percent.is_finite() || !(0.0..=100.0).contains(&strength_percent) {
        return Err("Strength must be between 0 and 100 percent".into());
    }
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let clock = ProjectClock::new(&inner.project.properties)?;
    let mut changed = false;
    match &scope {
        NoteScope::Track { track_index } => {
            let track = inner
                .project
                .tracks
                .get_mut(*track_index)
                .ok_or_else(|| format!("Track {} out of range", track_index))?;
            let plans = track_plans(track, notes, &clock)?;
            check_grid_fits(&plans, grid_quarters)?;
            for plan in plans {
                let phrase = &mut track.phrases[plan.phrase_index];
                if quantise_list(
                    &mut phrase.events,
                    &plan.selected,
                    plan.span,
                    grid_quarters,
                    strength_percent,
                ) {
                    sort_notes(&mut phrase.events);
                    changed = true;
                }
            }
        }
        NoteScope::Pattern { pattern_name } => {
            let pattern = inner
                .project
                .patterns
                .iter_mut()
                .find(|pattern| pattern.name == *pattern_name)
                .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
            let plan = pattern_plan(&pattern.events, notes)?;
            check_grid_fits(std::slice::from_ref(&plan), grid_quarters)?;
            if quantise_list(
                &mut pattern.events,
                &plan.selected,
                plan.span,
                grid_quarters,
                strength_percent,
            ) {
                sort_notes(&mut pattern.events);
                changed = true;
            }
        }
    }
    if changed {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new("quantise notes", UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

/// Nudge starts and velocities by a seeded per-note amount; one undo entry.
#[tauri::command]
pub fn humanise_notes(
    scope: NoteScope,
    notes: Vec<NoteRef>,
    timing_ticks: u32,
    velocity_spread: u8,
    seed: u64,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    humanise_notes_at(&state, scope, &notes, timing_ticks, velocity_spread, seed)
}

pub(crate) fn humanise_notes_at(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteRef],
    timing_ticks: u32,
    velocity_spread: u8,
    seed: u64,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let clock = ProjectClock::new(&inner.project.properties)?;
    let mut changed = false;
    match &scope {
        NoteScope::Track { track_index } => {
            let track = inner
                .project
                .tracks
                .get_mut(*track_index)
                .ok_or_else(|| format!("Track {} out of range", track_index))?;
            let plans = track_plans(track, notes, &clock)?;
            for plan in plans {
                let phrase = &mut track.phrases[plan.phrase_index];
                if humanise_list(
                    &mut phrase.events,
                    &plan.selected,
                    plan.span,
                    plan.phrase_index,
                    timing_ticks,
                    velocity_spread,
                    seed,
                ) {
                    sort_notes(&mut phrase.events);
                    changed = true;
                }
            }
        }
        NoteScope::Pattern { pattern_name } => {
            let pattern = inner
                .project
                .patterns
                .iter_mut()
                .find(|pattern| pattern.name == *pattern_name)
                .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
            let plan = pattern_plan(&pattern.events, notes)?;
            if humanise_list(
                &mut pattern.events,
                &plan.selected,
                plan.span,
                0,
                timing_ticks,
                velocity_spread,
                seed,
            ) {
                sort_notes(&mut pattern.events);
                changed = true;
            }
        }
    }
    if changed {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new("humanise notes", UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

/// Extend selected notes to their next start, or by `amount_beats`; one undo entry.
#[tauri::command]
pub fn legato_notes(
    scope: NoteScope,
    notes: Vec<NoteRef>,
    amount_beats: Option<f64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    legato_notes_at(&state, scope, &notes, amount_beats)
}

pub(crate) fn legato_notes_at(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteRef],
    amount_beats: Option<f64>,
) -> Result<ProjectSnapshot, String> {
    if let Some(amount) = amount_beats
        && (!amount.is_finite() || amount <= 0.0 || amount > 1024.0)
    {
        return Err("Legato amount must be finite and within (0, 1024] quarters".into());
    }
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let clock = ProjectClock::new(&inner.project.properties)?;
    let mut changed = false;
    match &scope {
        NoteScope::Track { track_index } => {
            let track = inner
                .project
                .tracks
                .get_mut(*track_index)
                .ok_or_else(|| format!("Track {} out of range", track_index))?;
            let plans = track_plans(track, notes, &clock)?;
            for plan in plans {
                let phrase = &mut track.phrases[plan.phrase_index];
                if legato_list(&mut phrase.events, &plan.selected, plan.span, amount_beats) {
                    sort_notes(&mut phrase.events);
                    changed = true;
                }
            }
        }
        NoteScope::Pattern { pattern_name } => {
            let pattern = inner
                .project
                .patterns
                .iter_mut()
                .find(|pattern| pattern.name == *pattern_name)
                .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
            let plan = pattern_plan(&pattern.events, notes)?;
            if legato_list(&mut pattern.events, &plan.selected, plan.span, amount_beats) {
                sort_notes(&mut pattern.events);
                changed = true;
            }
        }
    }
    if changed {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new("legato notes", UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

// ============================================================================
// Velocity lane tools
// ============================================================================

/// One selected note's loudness and place in time, before a velocity gesture.
struct Loudness {
    phrase_index: usize,
    note_index: usize,
    start_beat: f64,
    /// The velocity the note sounds at: exact value, written dynamic, or default.
    velocity: u8,
}

impl Loudness {
    /// `start_beat` is absolute: a phrase offset moves it, so a gesture over
    /// several phrases orders and interpolates in project time.
    fn of(phrase_index: usize, note_index: usize, start_beat: f64, note: &NoteEvent) -> Self {
        Self {
            phrase_index,
            note_index,
            start_beat,
            velocity: base_velocity(note),
        }
    }
}

/// Every selected note of a track, in time order.
fn track_loudness(
    track: &ast::Track,
    notes: &[NoteRef],
    clock: &ProjectClock,
) -> Result<Vec<Loudness>, String> {
    check_phrase_refs(track.phrases.len(), notes)?;
    let mut list = Vec::new();
    for (phrase_index, phrase) in track.phrases.iter().enumerate() {
        let selected = note_indices(notes, Some(phrase_index));
        if selected.is_empty() {
            continue;
        }
        check_note_refs(phrase.events.len(), &selected)?;
        let offset = phrase
            .time_range
            .as_ref()
            .map(|range| clock.quarter_notes_at(&range.start))
            .transpose()?
            .unwrap_or(0.0);
        for index in selected {
            let note = &phrase.events[index];
            list.push(Loudness::of(
                phrase_index,
                index,
                offset + note.start_beat,
                note,
            ));
        }
    }
    list.sort_by(|a, b| {
        a.start_beat
            .total_cmp(&b.start_beat)
            .then(a.phrase_index.cmp(&b.phrase_index))
            .then(a.note_index.cmp(&b.note_index))
    });
    Ok(list)
}

/// Every selected note of a pattern, in time order.
fn pattern_loudness(events: &[NoteEvent], notes: &[NoteRef]) -> Result<Vec<Loudness>, String> {
    let selected = note_indices(notes, None);
    check_note_refs(events.len(), &selected)?;
    let mut list: Vec<Loudness> = selected
        .into_iter()
        .map(|index| Loudness::of(0, index, events[index].start_beat, &events[index]))
        .collect();
    list.sort_by(|a, b| {
        a.start_beat
            .total_cmp(&b.start_beat)
            .then(a.note_index.cmp(&b.note_index))
    });
    Ok(list)
}

/// The selected notes of either scope, in time order.
fn selected_loudness(
    project: &ast::Project,
    scope: &NoteScope,
    notes: &[NoteRef],
    clock: &ProjectClock,
) -> Result<Vec<Loudness>, String> {
    match scope {
        NoteScope::Track { track_index } => {
            let track = project
                .tracks
                .get(*track_index)
                .ok_or_else(|| format!("Track {track_index} out of range"))?;
            track_loudness(track, notes, clock)
        }
        NoteScope::Pattern { pattern_name } => {
            let pattern = project
                .patterns
                .iter()
                .find(|pattern| pattern.name == *pattern_name)
                .ok_or_else(|| format!("Pattern '{pattern_name}' not found"))?;
            pattern_loudness(&pattern.events, notes)
        }
    }
}

/// A velocity rounded and held inside the MIDI range.
fn clamp_velocity(velocity: f64) -> u8 {
    velocity.round().clamp(1.0, 127.0) as u8
}

/// A straight line in time from the first selected velocity to the last.
fn ramp_targets(list: &[Loudness]) -> Result<Vec<u8>, String> {
    if list.len() < 2 {
        return Err("A ramp needs at least two notes".into());
    }
    let first = &list[0];
    let last = &list[list.len() - 1];
    let span = last.start_beat - first.start_beat;
    if span <= 1e-9 {
        return Err("A ramp needs notes at two different starts".into());
    }
    Ok(list
        .iter()
        .map(|note| {
            let fraction = (note.start_beat - first.start_beat) / span;
            clamp_velocity(
                f64::from(first.velocity)
                    + (f64::from(last.velocity) - f64::from(first.velocity)) * fraction,
            )
        })
        .collect())
}

/// Each velocity multiplied by `percent`, so 50 % halves the distance to zero.
fn scale_targets(list: &[Loudness], percent: f64) -> Result<Vec<u8>, String> {
    if !percent.is_finite() || percent <= 0.0 || percent > 1000.0 {
        return Err("Scale must be a percentage in (0, 1000]".into());
    }
    Ok(list
        .iter()
        .map(|note| clamp_velocity(f64::from(note.velocity) * percent / 100.0))
        .collect())
}

/// Each velocity moved halfway toward `target`; a note already there keeps it.
fn compress_targets(list: &[Loudness], target: u8) -> Result<Vec<u8>, String> {
    if target == 0 || target > 127 {
        return Err("Compress target must be 1-127".into());
    }
    let target = f64::from(target);
    Ok(list
        .iter()
        .map(|note| {
            let current = f64::from(note.velocity);
            clamp_velocity(current + (target - current) * 0.5)
        })
        .collect())
}

/// Apply one velocity gesture to the selection and record one undo entry.
fn write_velocity_gesture(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteRef],
    targets: impl FnOnce(&[Loudness]) -> Result<Vec<u8>, String>,
    label: &str,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let clock = ProjectClock::new(&inner.project.properties)?;
    // Every refusal happens before the first edit, so a rejected gesture cannot
    // leave half its notes changed without an undo entry.
    let list = selected_loudness(&inner.project, &scope, notes, &clock)?;
    let values = targets(&list)?;
    let mut changed = false;
    match &scope {
        NoteScope::Track { track_index } => {
            let track = inner
                .project
                .tracks
                .get_mut(*track_index)
                .ok_or_else(|| format!("Track {track_index} out of range"))?;
            for (note, &velocity) in list.iter().zip(&values) {
                if note.velocity == velocity {
                    continue;
                }
                track.phrases[note.phrase_index].events[note.note_index].velocity = Some(velocity);
                changed = true;
            }
        }
        NoteScope::Pattern { pattern_name } => {
            let pattern = inner
                .project
                .patterns
                .iter_mut()
                .find(|pattern| pattern.name == *pattern_name)
                .ok_or_else(|| format!("Pattern '{pattern_name}' not found"))?;
            for (note, &velocity) in list.iter().zip(&values) {
                if note.velocity == velocity {
                    continue;
                }
                pattern.events[note.note_index].velocity = Some(velocity);
                changed = true;
            }
        }
    }
    if changed {
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new(label, UndoScope::Project),
        )?;
    }
    Ok(snapshot(&inner))
}

/// Ramp the selection from its first velocity to its last, in time order.
#[tauri::command]
pub fn ramp_note_velocity(
    scope: NoteScope,
    notes: Vec<NoteRef>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    ramp_note_velocity_at(&state, scope, &notes)
}

pub(crate) fn ramp_note_velocity_at(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteRef],
) -> Result<ProjectSnapshot, String> {
    write_velocity_gesture(state, scope, notes, ramp_targets, "ramp note velocity")
}

/// Scale every selected velocity by a percentage.
#[tauri::command]
pub fn scale_note_velocity(
    scope: NoteScope,
    notes: Vec<NoteRef>,
    percent: f64,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    scale_note_velocity_at(&state, scope, &notes, percent)
}

pub(crate) fn scale_note_velocity_at(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteRef],
    percent: f64,
) -> Result<ProjectSnapshot, String> {
    write_velocity_gesture(
        state,
        scope,
        notes,
        |list| scale_targets(list, percent),
        "scale note velocity",
    )
}

/// Move every selected velocity halfway toward `target`.
#[tauri::command]
pub fn compress_note_velocity(
    scope: NoteScope,
    notes: Vec<NoteRef>,
    target: u8,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    compress_note_velocity_at(&state, scope, &notes, target)
}

pub(crate) fn compress_note_velocity_at(
    state: &AppState,
    scope: NoteScope,
    notes: &[NoteRef],
    target: u8,
) -> Result<ProjectSnapshot, String> {
    write_velocity_gesture(
        state,
        scope,
        notes,
        |list| compress_targets(list, target),
        "compress note velocity",
    )
}

// ============================================================================
// Pattern management
// ============================================================================

/// Add a named pattern.
#[tauri::command]
pub fn add_pattern(
    name: String,
    instrument_category: String,
    instrument_name: String,
    preset: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Pattern name must not be empty".into());
    }
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();

    if inner.project.patterns.iter().any(|p| p.name == name) {
        return Err(format!("Pattern '{}' already exists", name));
    }

    let pattern = ast::PatternDef {
        name,
        instrument: ast::InstrumentPath {
            category: instrument_category,
            name: instrument_name,
        },
        preset,
        engine: None,
        transpose: None,
        swing: None,
        bars: None,
        effects: vec![],
        events: vec![],
        pedals: vec![],
        hairpins: Vec::new(),
        hits: vec![],
        color: None,
    };

    inner.project.patterns.push(pattern);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("pattern add", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Remove a named pattern.
#[tauri::command]
pub fn remove_pattern(
    pattern_name: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    inner.project.patterns.retain(|p| p.name != pattern_name);
    // Remove any clips referencing this pattern
    for track in &mut inner.project.tracks {
        let kept: Vec<ast::Clip> = track
            .pattern_clips()
            .filter(|clip| clip.pattern != pattern_name)
            .cloned()
            .collect();
        track.set_pattern_clips(kept);
    }
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("pattern remove", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Rename a top-level pattern and every clip that references it, in one edit.
#[tauri::command]
pub fn rename_pattern(
    pattern_name: String,
    new_name: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    rename_pattern_blocking(&state, &pattern_name, &new_name)
}

pub(crate) fn rename_pattern_blocking(
    state: &AppState,
    pattern_name: &str,
    new_name: &str,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    commit_validated_edit(
        &mut inner,
        UndoEdit::new("pattern rename", UndoScope::Project),
        |next| rename_pattern_in(next, pattern_name, new_name),
    )?;
    Ok(snapshot(&inner))
}

/// The AST edit: the declaration takes the new name as one string, so quotes,
/// spaces and unicode need no rewriting, and every track's clips follow it.
/// Audio clips and their tune selectors are untouched.
pub(crate) fn rename_pattern_in(
    project: &mut ast::Project,
    pattern_name: &str,
    new_name: &str,
) -> Result<(), String> {
    let name = new_name.trim();
    if name.is_empty() {
        return Err("A pattern name must not be empty".into());
    }
    let index = project
        .patterns
        .iter()
        .position(|pattern| pattern.name == pattern_name)
        .ok_or_else(|| format!("Pattern '{pattern_name}' not found"))?;
    if name != pattern_name && project.patterns.iter().any(|pattern| pattern.name == name) {
        return Err(format!("A pattern named '{name}' already exists"));
    }
    project.patterns[index].name = name.to_string();
    for track in &mut project.tracks {
        for clip in track.pattern_clips_mut() {
            if clip.pattern == pattern_name {
                clip.pattern = name.to_string();
            }
        }
    }
    Ok(())
}

// ============================================================================
// Pattern property editing
// ============================================================================

/// Set a pattern transpose.
#[tauri::command]
pub fn set_pattern_transpose(
    pattern_name: String,
    transpose: i8,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let pattern = inner
        .project
        .patterns
        .iter_mut()
        .find(|p| p.name == pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
    pattern.transpose = Some(transpose);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("pattern transpose", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Set a pattern swing amount.
#[tauri::command]
pub fn set_pattern_swing(
    pattern_name: String,
    swing: Option<f64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_pattern_swing_inner(&mut inner, &pattern_name, swing)?;
    Ok(snapshot(&inner))
}

pub(crate) fn set_pattern_swing_inner(
    inner: &mut crate::state::AppStateInner,
    pattern_name: &str,
    swing: Option<f64>,
) -> Result<(), String> {
    let index = inner
        .project
        .patterns
        .iter()
        .position(|pattern| pattern.name == pattern_name)
        .ok_or_else(|| format!("Pattern '{pattern_name}' not found"))?;
    if inner.project.patterns[index].swing == swing {
        return Ok(());
    }
    commit_validated_edit(
        inner,
        UndoEdit::new("pattern swing", UndoScope::Project),
        |next| {
            next.patterns[index].swing = swing;
            Ok(())
        },
    )
}

/// Set the grid the project swing percentage moves.
#[tauri::command]
pub fn set_project_swing_subdivision(
    subdivision: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_project_swing_subdivision_inner(&mut inner, &subdivision)?;
    Ok(snapshot(&inner))
}

pub(crate) fn set_project_swing_subdivision_inner(
    inner: &mut crate::state::AppStateInner,
    subdivision: &str,
) -> Result<(), String> {
    let next = match subdivision {
        "8th" => oc_dsl::ast::SwingSubdivision::Eighth,
        "16th" => oc_dsl::ast::SwingSubdivision::Sixteenth,
        other => return Err(format!("swing subdivision '{other}' is not 8th or 16th")),
    };
    if inner.project.properties.swing_subdivision == next {
        return Ok(());
    }
    commit_validated_edit(
        inner,
        UndoEdit::new("swing subdivision", UndoScope::Project),
        |project| {
            project.properties.swing_subdivision = next;
            Ok(())
        },
    )
}

/// Set a pattern display color.
#[tauri::command]
pub fn set_pattern_color(
    pattern_name: String,
    color: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let pattern = inner
        .project
        .patterns
        .iter_mut()
        .find(|p| p.name == pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
    pattern.color = Some(color);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("pattern color", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

// ============================================================================
// Pattern length
// ============================================================================

/// Set a pattern length in bars.
#[tauri::command]
pub fn set_pattern_bars(
    pattern_name: String,
    bars: Option<f64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    set_pattern_bars_at(&state, &pattern_name, bars)?;
    let inner = state.lock_inner();
    Ok(snapshot(&inner))
}

/// Declare a pattern's length without touching its written content. Notes past
/// the new end stay in the document and the roll marks them out of range.
pub(crate) fn set_pattern_bars_at(
    state: &AppState,
    pattern_name: &str,
    bars: Option<f64>,
) -> Result<(), String> {
    if let Some(bars) = bars
        && (!bars.is_finite() || bars <= 0.0)
    {
        return Err("A pattern length must be a finite number of bars above zero".into());
    }
    let mut inner = state.lock_inner();
    commit_validated_edit(
        &mut inner,
        UndoEdit::new("pattern length", UndoScope::Project),
        |next| {
            let pattern = next
                .patterns
                .iter_mut()
                .find(|pattern| pattern.name == pattern_name)
                .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
            pattern.bars = bars;
            Ok(())
        },
    )
}

/// Trim pattern content to its bar length.
#[tauri::command]
pub fn trim_pattern_to_bars(
    pattern_name: String,
    bars: f64,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    trim_pattern_to_bars_at(&state, &pattern_name, bars)?;
    let inner = state.lock_inner();
    Ok(snapshot(&inner))
}

/// Declare a shorter length and cut what it no longer covers, in one undo entry.
pub(crate) fn trim_pattern_to_bars_at(
    state: &AppState,
    pattern_name: &str,
    bars: f64,
) -> Result<(), String> {
    if !bars.is_finite() || bars <= 0.0 {
        return Err("A pattern length must be a finite number of bars above zero".into());
    }
    let end = bars * 4.0;
    let mut inner = state.lock_inner();
    commit_validated_edit(
        &mut inner,
        UndoEdit::new("pattern trim", UndoScope::Project),
        |next| {
            let pattern = next
                .patterns
                .iter_mut()
                .find(|pattern| pattern.name == pattern_name)
                .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
            pattern.bars = Some(bars);
            pattern.events.retain_mut(|event| {
                if event.start_beat >= end - 1e-9 {
                    return false;
                }
                let remaining = end - event.start_beat;
                let written = performance_gate_quarters(&event.modifiers)
                    .ok()
                    .flatten()
                    .unwrap_or(event.duration.to_beats());
                if written > remaining + 1e-9 {
                    event.modifiers.retain(|(key, _)| key != "gate_quarters");
                    match editor_duration(remaining) {
                        Ok(duration) => event.duration = duration,
                        Err(_) => event.modifiers.push((
                            "gate_quarters".to_string(),
                            ParamValue::Number(remaining.max(1.0 / 48.0)),
                        )),
                    }
                }
                true
            });
            pattern
                .hits
                .retain(|hit| hit.written_quarters() < end - 1e-9);
            Ok(())
        },
    )
}

// ============================================================================
// Pattern hit editing
// ============================================================================

#[expect(
    clippy::too_many_arguments,
    reason = "Tauri preserves the flat IPC argument keys consumed by existing clients"
)]
/// Add one hit to a pattern drum lane.
#[tauri::command]
pub fn add_pattern_hit(
    pattern_name: String,
    drum: String,
    beat: u32,
    subdivision: u32,
    dynamics: Option<String>,
    offset_beats: Option<f64>,
    grid_offset_beats: Option<f64>,
    midi_note: Option<u8>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    add_pattern_hit_at(
        &state,
        &pattern_name,
        &drum,
        beat,
        subdivision,
        dynamics.as_deref(),
        HitExtras {
            offset_beats,
            grid_offset_beats,
            midi_note,
        },
    )?;
    let inner = state.lock_inner();
    Ok(snapshot(&inner))
}

/// The values a new hit writes beside its `beat:subdivision` position.
#[derive(Clone, Copy, Default)]
pub(crate) struct HitExtras {
    /// Expressive microtiming, in quarter notes.
    pub offset_beats: Option<f64>,
    /// Authored grid position relative to the sixteenth anchor.
    pub grid_offset_beats: Option<f64>,
    /// Note the hit plays, for a hit on a pad the kit does not name.
    pub midi_note: Option<u8>,
}

/// Widest microtiming a hit may carry, matching the engine and the validator.
const HIT_OFFSET_LIMIT: f64 = 0.125;

pub(crate) fn add_pattern_hit_at(
    state: &AppState,
    pattern_name: &str,
    drum: &str,
    beat: u32,
    subdivision: u32,
    dynamics: Option<&str>,
    extras: HitExtras,
) -> Result<(), String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let pattern = inner
        .project
        .patterns
        .iter_mut()
        .find(|p| p.name == pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
    if beat == 0 {
        return Err("a hit starts at beat 1; beat 0 is not a written position".into());
    }
    if !is_drum_name(drum) {
        return Err(format!(
            "'{drum}' is not a drum name; a hit writes an identifier such as kick or pad_50"
        ));
    }
    let mut modifiers: Vec<(String, ParamValue)> = Vec::new();
    let anchor = f64::from(beat - 1) + f64::from(subdivision) * 0.25;
    for (key, offset) in [
        ("grid_offset_beats", extras.grid_offset_beats),
        ("timing_offset_beats", extras.offset_beats),
    ] {
        if let Some(offset) = offset.filter(|offset| *offset != 0.0) {
            if !offset.is_finite() || offset.abs() > HIT_OFFSET_LIMIT {
                return Err(format!(
                    "{key} of {offset} quarters is outside ±{HIT_OFFSET_LIMIT}"
                ));
            }
            modifiers.push((key.to_string(), ParamValue::Number(offset)));
        }
    }
    let written = anchor + extras.grid_offset_beats.unwrap_or_default();
    let start = written + extras.offset_beats.unwrap_or_default();
    if written < 0.0 || start < 0.0 {
        return Err(format!(
            "a hit at {start} quarters starts before the pattern"
        ));
    }
    if extras.grid_offset_beats.is_some() {
        refuse_outside_pattern(pattern, written)?;
    }
    if let Some(note) = extras.midi_note {
        if note > 127 {
            return Err(format!("note {note} is outside the MIDI range 0..=127"));
        }
        modifiers.push(("midi_note".to_string(), ParamValue::Number(f64::from(note))));
    }
    refuse_outside_pattern(pattern, start)?;
    let hit = HitEvent {
        drum: drum.to_string(),
        position: BeatPosition { beat, subdivision },
        dynamics: dynamics.map(parse_dynamics).transpose()?,
        velocity: None,
        articulation: None,
        modifiers,
    };
    pattern.hits.push(hit);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("drum hit add", UndoScope::Project),
    )?;
    Ok(())
}

/// True when `name` parses back as the identifier a hit's DSL line writes.
fn is_drum_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Remove one hit from a pattern drum lane.
#[tauri::command]
pub fn remove_pattern_hit(
    pattern_name: String,
    hit_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    remove_pattern_hit_at(&state, &pattern_name, hit_index)?;
    let inner = state.lock_inner();
    Ok(snapshot(&inner))
}

/// Remove the hit at `hit_index` — the index the pattern holds, not the index
/// a filtered view happens to draw it at.
pub(crate) fn remove_pattern_hit_at(
    state: &AppState,
    pattern_name: &str,
    hit_index: usize,
) -> Result<(), String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let pattern = inner
        .project
        .patterns
        .iter_mut()
        .find(|p| p.name == pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
    if hit_index >= pattern.hits.len() {
        return Err(format!("Hit {} out of range", hit_index));
    }
    pattern.hits.remove(hit_index);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("drum hit remove", UndoScope::Project),
    )?;
    Ok(())
}

/// Move one hit in a pattern drum lane.
#[tauri::command]
pub fn move_pattern_hit(
    pattern_name: String,
    hit_index: usize,
    beat: u32,
    subdivision: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let pattern = inner
        .project
        .patterns
        .iter_mut()
        .find(|p| p.name == pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
    let hit = pattern
        .hits
        .get_mut(hit_index)
        .ok_or_else(|| format!("Hit {} out of range", hit_index))?;
    hit.position = BeatPosition { beat, subdivision };
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("drum hit move", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}
