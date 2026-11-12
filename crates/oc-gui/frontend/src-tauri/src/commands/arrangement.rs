//! Timeline clips, audio clips, performance events, and clip movement.

use std::collections::{BTreeSet, HashMap, HashSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tauri::State;

use oc_dsl::ast;
use oc_dsl::ast::GainEnvelopePoint;
use oc_dsl::timing::ProjectClock;

use crate::state::AppState;
use crate::state::{UndoEdit, UndoScope};

use super::shared::{ProjectSnapshot, commit_validated_edit, resync_with_undo, snapshot};

/// Replace one performance event at an expected index.
#[tauri::command]
pub fn edit_performance_event(
    track_index: usize,
    track_name: String,
    event_index: usize,
    expected: oc_dsl::performance::PerformanceEvent,
    replacement: oc_dsl::performance::PerformanceEvent,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    apply_performance_event_edit(
        &mut inner,
        track_index,
        &track_name,
        event_index,
        expected,
        replacement,
    )?;
    Ok(snapshot(&inner))
}

pub(crate) fn apply_performance_event_edit(
    inner: &mut crate::state::AppStateInner,
    track_index: usize,
    track_name: &str,
    event_index: usize,
    expected: oc_dsl::performance::PerformanceEvent,
    replacement: oc_dsl::performance::PerformanceEvent,
) -> Result<(), String> {
    let mut next = inner.project.clone();
    let track = next
        .tracks
        .get_mut(track_index)
        .ok_or("Track no longer exists")?;
    if track.name != track_name {
        return Err("Track changed; refresh before editing".into());
    }
    let timeline = track
        .performance
        .as_mut()
        .ok_or("Track has no explicit performance")?;
    let event = timeline
        .events
        .get_mut(event_index)
        .ok_or("Performance event no longer exists")?;
    if *event != expected {
        return Err("Performance event changed; refresh before editing".into());
    }
    if *event == replacement {
        return Ok(());
    }
    *event = replacement;
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid performance edit: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(
        inner,
        old,
        UndoEdit::new("performance event", UndoScope::Project),
    )?;
    Ok(())
}
// ============================================================================
// Clip management
// ============================================================================

#[tauri::command]
// Tauri command arity is the frontend IPC contract.
#[allow(clippy::too_many_arguments)]
/// Place a pattern clip on a track.
pub fn add_clip(
    track_index: usize,
    pattern_name: String,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    end_bar: u32,
    end_beat: u32,
    end_sub: u32,
    offset_quarters: Option<f64>,
    loop_quarters: Option<f64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    add_clip_inner(
        &mut inner,
        track_index,
        &pattern_name,
        ast::BarBeat::new(start_bar, start_beat, start_sub),
        ast::BarBeat::new(end_bar, end_beat, end_sub),
        offset_quarters,
        loop_quarters,
    )?;
    Ok(snapshot(&inner))
}

/// Place a pattern clip, optionally with the playback window it should play.
///
/// `offset_quarters` and `loop_quarters` are the slip and loop a copy or a
/// split carries; a refused window leaves the document untouched.
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_clip_inner(
    inner: &mut crate::state::AppStateInner,
    track_index: usize,
    pattern_name: &str,
    start: ast::BarBeat,
    end: ast::BarBeat,
    offset_quarters: Option<f64>,
    loop_quarters: Option<f64>,
) -> Result<(), String> {
    let pattern_name = pattern_name.trim().to_string();
    if pattern_name.is_empty() {
        return Err("Clip must reference a named pattern".into());
    }
    let offset = offset_quarters.unwrap_or(0.0);
    let loop_quarters = loop_quarters.unwrap_or(0.0);
    if !offset.is_finite() || offset < 0.0 {
        return Err("A slip offset must be zero or longer.".into());
    }
    if !loop_quarters.is_finite() || loop_quarters < 0.0 {
        return Err("A loop length must be zero or longer.".into());
    }

    let carried_window = offset > 0.0 || loop_quarters > 0.0;
    commit_validated_edit(
        inner,
        UndoEdit::new("add clip", UndoScope::Project),
        |project| {
            if !project.patterns.iter().any(|p| p.name == pattern_name) {
                return Err(format!("Pattern '{}' not found", pattern_name));
            }
            let track = project
                .tracks
                .get_mut(track_index)
                .ok_or_else(|| format!("Track {track_index} out of range"))?;
            let added = track.pattern_clip_count();
            track.push_pattern_clip(ast::Clip {
                pattern: pattern_name.clone(),
                time_range: ast::TimeRange { start, end },
                offset_quarters: offset,
                loop_quarters,
                id: None,
            });
            // A carried playback window has to satisfy the loop rules a loaded
            // document does. Name that rule only when the window is what breaks
            // the document; the validator speaks for everything else.
            if carried_window && let Err(errors) = oc_dsl::validate(project) {
                let mut plain = project.clone();
                if let Some(clip) = plain.tracks[track_index].pattern_clip_mut(added) {
                    clip.offset_quarters = 0.0;
                    clip.loop_quarters = 0.0;
                }
                if oc_dsl::validate(&plain).is_ok() {
                    return Err(format!("Invalid clip playback window: {errors:?}"));
                }
            }
            Ok(())
        },
    )
}

/// Remove a pattern clip from a track.
#[tauri::command]
pub fn remove_clip(
    track_index: usize,
    clip_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    remove_clip_inner(&mut inner, track_index, clip_index)?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("remove clip", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

pub(crate) fn remove_clip_inner(
    inner: &mut crate::state::AppStateInner,
    track_index: usize,
    clip_index: usize,
) -> Result<(), String> {
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    if clip_index >= track.pattern_clip_count() {
        return Err(format!("Clip {} out of range", clip_index));
    }
    track.remove_pattern_clip(clip_index);
    // A group that named the clip must not keep a dangling member.
    oc_dsl::groups::reconcile(&mut inner.project);
    Ok(())
}

/// Refusals a pattern-clip split repeats verbatim.
pub const SPLIT_INSIDE_CLIP: &str = "The split point must be inside the clip.";
/// Split refusal: the right half starts past pattern content.
pub const SPLIT_PAST_CONTENT: &str =
    "The right half would start past the pattern's content; move the split earlier.";
/// Split refusal: the cut is not a loop boundary.
pub const SPLIT_LOOP_SEAM: &str =
    "This split cuts a loop that the right half cannot repeat; move the split to a loop boundary.";
/// Split refusal: the repeat exceeds pattern content.
pub const SPLIT_LOOP_CONTENT: &str =
    "This split would repeat content the pattern does not have; move the split to a loop boundary.";
/// Split refusal: the audio cut is not a loop boundary.
pub const SPLIT_AUDIO_LOOP_SEAM: &str =
    "This split cuts a loop that the right half cannot repeat; move the split to a loop boundary.";
/// Split refusal: the repeat exceeds the audio file.
pub const SPLIT_AUDIO_LOOP_PAST_MEDIA: &str =
    "This split would repeat media the file does not have; move the split to a loop boundary.";

/// Split a pattern clip at `at` in one undoable edit.
///
/// The right half resumes where the left stops, so a slipped or looped clip
/// keeps its playback window instead of restarting at the pattern head.
#[tauri::command]
pub fn split_clip(
    track_index: usize,
    clip_index: usize,
    at_bar: u32,
    at_beat: u32,
    at_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    split_clip_inner(
        &mut inner,
        track_index,
        clip_index,
        ast::BarBeat::new(at_bar, at_beat, at_sub),
    )?;
    Ok(snapshot(&inner))
}

/// A split the document cannot express leaves the project untouched.
pub(crate) fn split_clip_inner(
    inner: &mut crate::state::AppStateInner,
    track_index: usize,
    clip_index: usize,
    at: ast::BarBeat,
) -> Result<(), String> {
    commit_validated_edit(
        inner,
        UndoEdit::new("clip split", UndoScope::Project),
        |next| {
            let clock = ProjectClock::new(&next.properties)?;
            let clip = next
                .tracks
                .get(track_index)
                .and_then(|track| track.pattern_clip(clip_index))
                .ok_or_else(|| format!("Clip {clip_index} on track {track_index} out of range"))?
                .clone();
            let start = clock.quarter_notes_at(&clip.time_range.start)?;
            let end = clock.quarter_notes_at(&clip.time_range.end)?;
            let split = clock.quarter_notes_at(&at)?;
            if split <= start + 1e-9 || split >= end - 1e-9 {
                return Err(SPLIT_INSIDE_CLIP.into());
            }
            let content = next
                .patterns
                .iter()
                .find(|pattern| pattern.name == clip.pattern)
                .map_or(0.0, |pattern| {
                    if clip.loop_quarters > 1e-9 {
                        pattern_loop_end_quarters(pattern)
                    } else {
                        pattern.content_quarters()
                    }
                });
            let window = split_window(
                clip.offset_quarters,
                clip.loop_quarters,
                split - start,
                end - split,
                content,
            )?;
            let left = ast::Clip {
                pattern: clip.pattern.clone(),
                time_range: ast::TimeRange {
                    start: clip.time_range.start,
                    end: at,
                },
                offset_quarters: window.0,
                loop_quarters: window.1,
                id: clip.id.clone(),
            };
            let right = ast::Clip {
                pattern: clip.pattern.clone(),
                time_range: ast::TimeRange {
                    start: at,
                    end: clip.time_range.end,
                },
                offset_quarters: window.2,
                loop_quarters: window.3,
                id: None,
            };
            let track = next
                .tracks
                .get_mut(track_index)
                .ok_or_else(|| format!("Track {track_index} out of range"))?;
            let mut clips: Vec<ast::Clip> = track.pattern_clips().cloned().collect();
            clips[clip_index] = left;
            clips.insert(clip_index + 1, right);
            track.set_pattern_clips(clips);
            Ok(())
        },
    )
}

/// The playback window each half keeps when a clip splits `elapsed` quarters in.
///
/// A loop repeats the content window `[offset, offset + loop)` and restarts at
/// `offset` at every seam, so the right half resumes at the content position the
/// loop reaches rather than at the pattern head. A loop longer than a half
/// repeats nowhere inside it, and that half plays one pass; a seam that would
/// still fall inside the right half has no expressible loop and is refused.
///
/// Returns `(left_offset, left_loop, right_offset, right_loop)`.
fn split_window(
    offset: f64,
    loop_quarters: f64,
    elapsed: f64,
    right_length: f64,
    content: f64,
) -> Result<(f64, f64, f64, f64), String> {
    let looping = loop_quarters > 1e-9;
    let wrapped = if looping {
        let phase = elapsed.rem_euclid(loop_quarters);
        if phase <= 1e-9 || loop_quarters - phase <= 1e-9 {
            0.0
        } else {
            phase
        }
    } else {
        0.0
    };
    let right_offset = if looping {
        offset + wrapped
    } else {
        offset + elapsed
    };
    let left_loop = if looping && loop_quarters < elapsed - 1e-9 {
        loop_quarters
    } else {
        0.0
    };
    let right_loop = if looping && loop_quarters < right_length - 1e-9 {
        loop_quarters
    } else {
        0.0
    };
    if looping && right_loop == 0.0 {
        // The source still wraps at the next seam; without a loop the half
        // would keep playing past that seam instead of repeating.
        let to_seam = if wrapped > 1e-9 {
            loop_quarters - wrapped
        } else {
            loop_quarters
        };
        if right_length > to_seam + 1e-9 {
            return Err(SPLIT_LOOP_SEAM.into());
        }
    }
    if right_loop > 0.0 && right_offset + right_loop > content + 1e-9 {
        return Err(SPLIT_LOOP_CONTENT.into());
    }
    if right_loop > 0.0 && wrapped > 1e-9 {
        return Err(SPLIT_LOOP_SEAM.into());
    }
    if right_loop == 0.0 && content > 0.0 && right_offset >= content - 1e-9 {
        return Err(SPLIT_PAST_CONTENT.into());
    }
    Ok((offset, left_loop, right_offset, right_loop))
}

/// One lane block of a timeline multi-selection; `kind` is
/// "clip", "phrase" or "pattern" and `index` addresses that list.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockSelection {
    pub kind: String,
    #[serde(default)]
    pub track_index: usize,
    pub index: usize,
}

/// Delete every selected lane block in one undo step.
#[tauri::command]
pub fn remove_blocks(
    blocks: Vec<BlockSelection>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    remove_blocks_inner(&mut inner, &blocks)?;
    Ok(snapshot(&inner))
}

pub(crate) fn remove_blocks_inner(
    inner: &mut crate::state::AppStateInner,
    blocks: &[BlockSelection],
) -> Result<(), String> {
    if blocks.is_empty() {
        return Ok(());
    }
    let mut next = inner.project.clone();
    let mut clips: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    let mut phrases: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    let mut patterns: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    for block in blocks {
        let track = next
            .tracks
            .get(block.track_index)
            .ok_or_else(|| format!("Track {} out of range", block.track_index))?;
        let (count, target) = match block.kind.as_str() {
            "clip" => (track.pattern_clip_count(), &mut clips),
            "phrase" => (track.phrases.len(), &mut phrases),
            "pattern" => (track.patterns.len(), &mut patterns),
            other => return Err(format!("Unknown timeline block kind '{other}'")),
        };
        if block.index >= count {
            return Err(format!("{} {} out of range", block.kind, block.index));
        }
        target
            .entry(block.track_index)
            .or_default()
            .insert(block.index);
    }
    // Descending indices keep earlier removals from shifting later ones.
    for (track_index, indices) in clips {
        if let Some(track) = next.tracks.get_mut(track_index) {
            for index in indices.iter().rev() {
                track.remove_pattern_clip(*index);
            }
        }
    }
    for (track_index, indices) in phrases {
        if let Some(track) = next.tracks.get_mut(track_index) {
            for index in indices.iter().rev() {
                track.phrases.remove(*index);
            }
        }
    }
    for (track_index, indices) in patterns {
        if let Some(track) = next.tracks.get_mut(track_index) {
            for index in indices.iter().rev() {
                track.patterns.remove(*index);
            }
        }
    }
    // A group that named a removed clip must not keep a dangling member.
    oc_dsl::groups::reconcile(&mut next);
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(
        inner,
        old,
        UndoEdit::new("remove blocks", UndoScope::Project),
    )?;
    Ok(())
}

/// Move a pattern clip to a new bar and beat.
#[tauri::command]
pub fn move_clip(
    track_index: usize,
    clip_index: usize,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    let clip = track
        .pattern_clip_mut(clip_index)
        .ok_or_else(|| format!("Clip {} out of range", clip_index))?;

    let start = ast::BarBeat {
        bar: start_bar,
        beat: start_beat,
        subdivision: start_sub,
    };
    clip.time_range = retimed_clip(&clock, clip, start)?;

    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("clip move", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Keep a clip's duration while moving its start, exact on the notated grid.
fn retimed_clip(
    clock: &oc_dsl::timing::ProjectClock,
    clip: &ast::Clip,
    start: ast::BarBeat,
) -> Result<ast::TimeRange, String> {
    retimed_range(clock, &clip.time_range, start)
}

/// Keep any timeline block's duration while moving its start.
pub(crate) fn retimed_range(
    clock: &ProjectClock,
    range: &ast::TimeRange,
    start: ast::BarBeat,
) -> Result<ast::TimeRange, String> {
    let duration = clock.quarter_notes_at(&range.end)? - clock.quarter_notes_at(&range.start)?;
    if duration <= 0.0 {
        return Err("Block duration must be positive".into());
    }
    Ok(ast::TimeRange {
        start,
        end: clock.bar_beat_at_quarter(clock.quarter_notes_at(&start)? + duration)?,
    })
}

/// Keep pasted block labels distinct from the blocks already on the track.
pub(crate) fn unique_block_name<'a>(base: &str, existing: impl Iterator<Item = &'a str>) -> String {
    let names: HashSet<&str> = existing.collect();
    if !names.contains(base) {
        return base.to_string();
    }
    for suffix in 2.. {
        let candidate = format!("{base} {suffix}");
        if !names.contains(candidate.as_str()) {
            return candidate;
        }
    }
    unreachable!("the suffix range is unbounded")
}

/// Keep a clip's start while moving its end; the end must stay after it.
pub(crate) fn resized_range(
    clock: &ProjectClock,
    range: &ast::TimeRange,
    end: ast::BarBeat,
) -> Result<ast::TimeRange, String> {
    let start = range.start;
    let quantized = ast::BarBeat {
        bar: end.bar,
        beat: end.beat,
        subdivision: end.subdivision.min(3),
    };
    let start_quarter = clock.quarter_notes_at(&start)?;
    if clock.quarter_notes_at(&quantized)? - start_quarter < 0.25 {
        return Err("Block duration must be at least a 16th note".into());
    }
    Ok(ast::TimeRange {
        start,
        end: quantized,
    })
}

/// Resize a named clip to a new end, keeping its start. One undo step.
#[tauri::command]
pub fn resize_clip(
    track_index: usize,
    clip_index: usize,
    end_bar: u32,
    end_beat: u32,
    end_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    let clip = track
        .pattern_clip_mut(clip_index)
        .ok_or_else(|| format!("Clip {} out of range", clip_index))?;
    clip.time_range = resized_range(
        &clock,
        &clip.time_range,
        ast::BarBeat {
            bar: end_bar,
            beat: end_beat,
            subdivision: end_sub,
        },
    )?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("clip resize", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Move a clip between tracks in one step; caller owns undo/resync.
pub(crate) fn move_clip_between_tracks(
    project: &mut ast::Project,
    from_track_index: usize,
    clip_index: usize,
    to_track_index: usize,
    start: ast::BarBeat,
) -> Result<(), String> {
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    if to_track_index >= project.tracks.len() {
        return Err(format!("Track {} out of range", to_track_index));
    }
    let mut clip = project
        .tracks
        .get(from_track_index)
        .and_then(|track| track.pattern_clip(clip_index))
        .cloned()
        .ok_or_else(|| {
            format!(
                "Clip {} out of range on track {}",
                clip_index, from_track_index
            )
        })?;
    clip.time_range = retimed_clip(&clock, &clip, start)?;
    if from_track_index == to_track_index {
        *project.tracks[from_track_index]
            .pattern_clip_mut(clip_index)
            .unwrap() = clip;
        return Ok(());
    }
    project.tracks[from_track_index].remove_pattern_clip(clip_index);
    project.tracks[to_track_index].push_pattern_clip(clip);
    Ok(())
}

/// Move a clip to another track at a new start, keeping its duration. One undo step.
#[tauri::command]
pub fn move_clip_to_track(
    from_track_index: usize,
    clip_index: usize,
    to_track_index: usize,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    move_clip_to_track_inner(
        &mut inner,
        from_track_index,
        clip_index,
        to_track_index,
        ast::BarBeat {
            bar: start_bar,
            beat: start_beat,
            subdivision: start_sub,
        },
    )?;
    Ok(snapshot(&inner))
}

/// A move the document cannot express leaves the project untouched.
///
/// This is the cross-track gate: a pattern clip never lands on a `performance:`
/// track, and a move the validator refuses keeps its source placement.
pub(crate) fn move_clip_to_track_inner(
    inner: &mut crate::state::AppStateInner,
    from_track_index: usize,
    clip_index: usize,
    to_track_index: usize,
    start: ast::BarBeat,
) -> Result<(), String> {
    commit_validated_edit(
        inner,
        UndoEdit::new("clip move", UndoScope::Project),
        |project| {
            move_clip_between_tracks(project, from_track_index, clip_index, to_track_index, start)
        },
    )
}

/// Move an inline phrase within its track at a new start, keeping its duration.
#[tauri::command]
pub fn move_phrase(
    track_index: usize,
    phrase_index: usize,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    move_phrase_inner(
        &mut inner,
        track_index,
        phrase_index,
        track_index,
        ast::BarBeat {
            bar: start_bar,
            beat: start_beat,
            subdivision: start_sub,
        },
    )?;
    Ok(snapshot(&inner))
}

/// A phrase move the document cannot express leaves the project untouched.
pub(crate) fn move_phrase_inner(
    inner: &mut crate::state::AppStateInner,
    from_track_index: usize,
    phrase_index: usize,
    to_track_index: usize,
    start: ast::BarBeat,
) -> Result<(), String> {
    commit_validated_edit(
        inner,
        UndoEdit::new("phrase move", UndoScope::Project),
        |project| {
            move_phrase_between_tracks(
                project,
                from_track_index,
                phrase_index,
                to_track_index,
                start,
            )
        },
    )
}

/// Move an inline phrase to another track in one step; caller owns undo/resync.
pub(crate) fn move_phrase_between_tracks(
    project: &mut ast::Project,
    from_track_index: usize,
    phrase_index: usize,
    to_track_index: usize,
    start: ast::BarBeat,
) -> Result<(), String> {
    let clock = ProjectClock::new(&project.properties)?;
    if to_track_index >= project.tracks.len() {
        return Err(format!("Track {} out of range", to_track_index));
    }
    let mut phrase = project
        .tracks
        .get(from_track_index)
        .and_then(|track| track.phrases.get(phrase_index))
        .cloned()
        .ok_or_else(|| {
            format!(
                "Phrase {} out of range on track {}",
                phrase_index, from_track_index
            )
        })?;
    let range = phrase
        .time_range
        .ok_or_else(|| format!("Phrase {phrase_index} has no time range"))?;
    phrase.time_range = Some(retimed_range(&clock, &range, start)?);
    if from_track_index == to_track_index {
        project.tracks[from_track_index].phrases[phrase_index] = phrase;
        return Ok(());
    }
    project.tracks[from_track_index]
        .phrases
        .remove(phrase_index);
    project.tracks[to_track_index].phrases.push(phrase);
    Ok(())
}

/// Move an inline phrase across tracks at a new start. One undo step.
#[tauri::command]
pub fn move_phrase_to_track(
    from_track_index: usize,
    phrase_index: usize,
    to_track_index: usize,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    move_phrase_inner(
        &mut inner,
        from_track_index,
        phrase_index,
        to_track_index,
        ast::BarBeat {
            bar: start_bar,
            beat: start_beat,
            subdivision: start_sub,
        },
    )?;
    Ok(snapshot(&inner))
}

/// Move an inline drum pattern within its track, keeping its duration and repeat.
#[tauri::command]
pub fn move_inline_pattern(
    track_index: usize,
    pattern_index: usize,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    move_inline_pattern_inner(
        &mut inner,
        track_index,
        pattern_index,
        track_index,
        ast::BarBeat {
            bar: start_bar,
            beat: start_beat,
            subdivision: start_sub,
        },
    )?;
    Ok(snapshot(&inner))
}

/// An inline pattern move the document cannot express leaves it untouched.
pub(crate) fn move_inline_pattern_inner(
    inner: &mut crate::state::AppStateInner,
    from_track_index: usize,
    pattern_index: usize,
    to_track_index: usize,
    start: ast::BarBeat,
) -> Result<(), String> {
    commit_validated_edit(
        inner,
        UndoEdit::new("pattern move", UndoScope::Project),
        |project| {
            move_inline_pattern_between_tracks(
                project,
                from_track_index,
                pattern_index,
                to_track_index,
                start,
            )
        },
    )
}

/// Move an inline drum pattern between tracks; caller owns undo/resync.
pub(crate) fn move_inline_pattern_between_tracks(
    project: &mut ast::Project,
    from_track_index: usize,
    pattern_index: usize,
    to_track_index: usize,
    start: ast::BarBeat,
) -> Result<(), String> {
    let clock = ProjectClock::new(&project.properties)?;
    if to_track_index >= project.tracks.len() {
        return Err(format!("Track {} out of range", to_track_index));
    }
    let mut pattern = project
        .tracks
        .get(from_track_index)
        .and_then(|track| track.patterns.get(pattern_index))
        .cloned()
        .ok_or_else(|| {
            format!(
                "Pattern {} out of range on track {}",
                pattern_index, from_track_index
            )
        })?;
    let range = pattern
        .time_range
        .ok_or_else(|| format!("Pattern {pattern_index} has no time range"))?;
    pattern.time_range = Some(retimed_range(&clock, &range, start)?);
    if from_track_index == to_track_index {
        project.tracks[from_track_index].patterns[pattern_index] = pattern;
        return Ok(());
    }
    project.tracks[from_track_index]
        .patterns
        .remove(pattern_index);
    project.tracks[to_track_index].patterns.push(pattern);
    Ok(())
}

/// Move an inline drum pattern across tracks at a new start. One undo step.
#[tauri::command]
pub fn move_inline_pattern_to_track(
    from_track_index: usize,
    pattern_index: usize,
    to_track_index: usize,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    move_inline_pattern_inner(
        &mut inner,
        from_track_index,
        pattern_index,
        to_track_index,
        ast::BarBeat {
            bar: start_bar,
            beat: start_beat,
            subdivision: start_sub,
        },
    )?;
    Ok(snapshot(&inner))
}

// ============================================================================
// Audio clips
// ============================================================================

/// Decode media peaks on a worker thread; the UI never touches the decoder.
#[tauri::command]
pub async fn audio_clip_peaks(file: String, state: State<'_, AppState>) -> Result<String, String> {
    let root = {
        let inner = state.lock_inner();
        inner
            .file_path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf)
    };
    let path = oc_engine::audio_clip::resolve_path(&file, root.as_deref())?;
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        oc_engine::media_cache::shared().pyramid_bytes(&path)
    })
    .await
    .map_err(|error| format!("Peak job failed: {error}"))??;
    Ok(STANDARD.encode(bytes))
}

pub(crate) fn audio_clip_source_path(
    state: &AppState,
    clip_index: usize,
) -> Result<std::path::PathBuf, String> {
    let inner = state.lock_inner();
    let clip = inner
        .project
        .audio_clip(clip_index)
        .ok_or_else(|| format!("Audio clip {clip_index} out of range"))?;
    let root = inner.file_path.as_deref().and_then(std::path::Path::parent);
    oc_engine::audio_clip::resolve_path(&clip.file, root)
}

pub(crate) fn measure_audio_clip_transients(
    path: &std::path::Path,
    source_start_seconds: f64,
    duration_seconds: f64,
    limit: usize,
) -> Result<oc_engine::audio_peaks::SourceTransients, String> {
    oc_engine::media_cache::shared()
        .decoded(path)?
        .pyramid()
        .source_transients(source_start_seconds, duration_seconds, limit)
}

/// Detect transients in an audio clip.
#[tauri::command]
pub async fn audio_clip_transients(
    clip_index: usize,
    source_start_seconds: f64,
    duration_seconds: f64,
    limit: usize,
    state: State<'_, AppState>,
) -> Result<oc_engine::audio_peaks::SourceTransients, String> {
    let path = audio_clip_source_path(state.inner(), clip_index)?;
    tauri::async_runtime::spawn_blocking(move || {
        measure_audio_clip_transients(&path, source_start_seconds, duration_seconds, limit)
    })
    .await
    .map_err(|error| format!("Transient analysis failed: {error}"))?
}

/// Warm peaks for every clip in the background on open or DSL update.
pub(crate) fn warm_audio_clip_peaks(project: &ast::Project, root: Option<&std::path::Path>) {
    let mut seen = std::collections::HashSet::new();
    let mut paths = Vec::new();
    for clip in project.audio_clips() {
        if let Ok(path) = oc_engine::audio_clip::resolve_path(&clip.file, root)
            && seen.insert(path.clone())
        {
            paths.push(path);
        }
    }
    if paths.is_empty() {
        return;
    }
    tauri::async_runtime::spawn_blocking(move || {
        for path in paths {
            if let Err(error) = oc_engine::media_cache::shared().pyramid_bytes(&path) {
                log::warn!("audio clip peaks: {}: {error}", path.display());
            }
        }
    });
}

/// Move an audio clip to a new musical position.
#[tauri::command]
pub fn move_audio_clip(
    clip_index: usize,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    track: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    move_audio_clip_inner(
        &mut inner,
        clip_index,
        ast::BarBeat::new(start_bar, start_beat, start_sub),
        track,
    )?;
    Ok(snapshot(&inner))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
/// Trim an audio clip start or end.
pub fn trim_audio_clip(
    clip_index: usize,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    end_bar: u32,
    end_beat: u32,
    end_sub: u32,
    edge: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    trim_audio_clip_edge_inner(
        &mut inner,
        clip_index,
        ast::BarBeat::new(start_bar, start_beat, start_sub),
        ast::BarBeat::new(end_bar, end_beat, end_sub),
        edge.as_deref(),
    )?;
    Ok(snapshot(&inner))
}

/// Split an audio clip at a musical position.
#[tauri::command]
pub fn split_audio_clip(
    clip_index: usize,
    at_bar: u32,
    at_beat: u32,
    at_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    split_audio_clip_inner(
        &mut inner,
        clip_index,
        ast::BarBeat::new(at_bar, at_beat, at_sub),
    )?;
    Ok(snapshot(&inner))
}

/// Remove an audio clip.
#[tauri::command]
pub fn remove_audio_clip(
    clip_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    remove_audio_clip_inner(&mut inner, clip_index)?;
    Ok(snapshot(&inner))
}

/// Choose `take` over one range: split at both edges and mute every other take.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn comp_audio_takes(
    track: String,
    take: u32,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    end_bar: u32,
    end_beat: u32,
    end_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    comp_audio_takes_inner(
        &mut inner,
        &track,
        take,
        ast::BarBeat::new(start_bar, start_beat, start_sub),
        ast::BarBeat::new(end_bar, end_beat, end_sub),
    )?;
    Ok(snapshot(&inner))
}

/// Mute or unmute every clip of one take in one undoable edit.
#[tauri::command]
pub fn set_audio_take_mute(
    track: String,
    take: u32,
    muted: bool,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_audio_take_mute_inner(&mut inner, &track, take, muted)?;
    Ok(snapshot(&inner))
}

/// The take auditioned through the live render thread, if any.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TakeSoloState {
    pub track: Option<String>,
    pub take: Option<u32>,
}

/// Audition one take. Session only: the document keeps its mute flags.
#[tauri::command]
pub fn set_take_solo(
    track: Option<String>,
    take: Option<u32>,
    state: State<AppState>,
) -> Result<TakeSoloState, String> {
    let mut inner = state.lock_inner();
    match (&track, take) {
        (None, None) => inner.audio.take_solo = None,
        (Some(name), Some(take)) => {
            if take == 0
                || !inner
                    .project
                    .audio_clips()
                    .iter()
                    .any(|clip| clip.track == *name && clip.take == Some(take))
            {
                return Err(format!("Track '{name}' has no take {take}."));
            }
            inner.audio.take_solo = Some((name.clone(), take));
        }
        _ => return Err("A take solo needs both a track and a take.".into()),
    }
    let project = inner.project.clone();
    inner.audio.preload_project(&project);
    Ok(TakeSoloState { track, take })
}

pub(crate) fn move_audio_clip_inner(
    inner: &mut crate::state::AppStateInner,
    clip_index: usize,
    start: ast::BarBeat,
    track: Option<String>,
) -> Result<(), String> {
    let mut next = inner.project.clone();
    let clock = oc_dsl::timing::ProjectClock::new(&next.properties)?;
    let clip = next
        .audio_clip_mut(clip_index)
        .ok_or_else(|| format!("Audio clip {clip_index} out of range"))?;
    let duration = clock.quarter_notes_at(&clip.time_range.end)?
        - clock.quarter_notes_at(&clip.time_range.start)?;
    clock.audio_clip_range_seconds(clip)?;
    let end = clock.bar_beat_at_quarter(clock.quarter_notes_at(&start)? + duration)?;
    clip.time_range = ast::TimeRange { start, end };
    if let Some(track) = track {
        clip.track = track.trim().to_string();
    }
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid audio clip move: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(
        inner,
        old,
        UndoEdit::new("audio clip move", UndoScope::Project),
    )?;
    Ok(())
}

/// Move the placement without trimming media or changing its elapsed duration.
#[tauri::command]
pub fn audio_clip_set_placement(
    clip_index: usize,
    start_seconds: Option<f64>,
    nudge_ms: Option<f64>,
    expected_revision: Option<u64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    guard_gui_clip_edit(&inner, expected_revision)?;
    set_audio_clip_placement_inner(&mut inner, clip_index, start_seconds, nudge_ms)?;
    Ok(snapshot(&inner))
}

pub(crate) fn set_audio_clip_placement_inner(
    inner: &mut crate::state::AppStateInner,
    clip_index: usize,
    start_seconds: Option<f64>,
    nudge_ms: Option<f64>,
) -> Result<(), String> {
    let mut next = inner.project.clone();
    let clock = ProjectClock::new(&next.properties)?;
    let clip = next
        .audio_clip_mut(clip_index)
        .ok_or_else(|| format!("Audio clip {clip_index} out of range"))?;
    let (start, end) = clock.audio_clip_range_seconds(clip)?;
    match (start_seconds, nudge_ms) {
        (Some(value), None) if value.is_finite() && value >= 0.0 => {
            if value == start {
                return Ok(());
            }
            // Derive absolute offsets from their anchors. Incremental deltas
            // can cancel to a negative epsilon when the requested start is zero.
            let anchor_start =
                clock.seconds_at_quarter(clock.quarter_notes_at(&clip.time_range.start)?)?;
            let anchor_end =
                clock.seconds_at_quarter(clock.quarter_notes_at(&clip.time_range.end)?)?;
            clip.placement.start_offset_seconds = value - anchor_start;
            clip.placement.end_offset_seconds = (value + (end - start)) - anchor_end;
        }
        (None, Some(value)) if value.is_finite() => {
            if value == 0.0 {
                return Ok(());
            }
            let delta = value / 1000.0;
            let target_start = start + delta;
            let cancellation_bound = 4.0 * f64::EPSILON * (start.abs() + delta.abs()).max(1.0);
            if target_start.abs() <= cancellation_bound {
                clip.placement.start_offset_seconds =
                    -clock.seconds_at_quarter(clock.quarter_notes_at(&clip.time_range.start)?)?;
            } else {
                clip.placement.start_offset_seconds += delta;
            }
            clip.placement.end_offset_seconds += delta;
        }
        _ => {
            return Err(
                "Supply exactly one finite start_seconds (zero or longer) or nudge_ms.".into(),
            );
        }
    }
    clock.audio_clip_range_seconds(clip)?;
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid clip placement: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(
        inner,
        old,
        UndoEdit::new("audio clip placement", UndoScope::Project),
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn trim_audio_clip_inner(
    inner: &mut crate::state::AppStateInner,
    clip_index: usize,
    start: ast::BarBeat,
    end: ast::BarBeat,
) -> Result<(), String> {
    trim_audio_clip_edge_inner(inner, clip_index, start, end, None)
}

pub(crate) fn trim_audio_clip_edge_inner(
    inner: &mut crate::state::AppStateInner,
    clip_index: usize,
    start: ast::BarBeat,
    end: ast::BarBeat,
    edge: Option<&str>,
) -> Result<(), String> {
    if !matches!(edge, None | Some("start" | "end")) {
        return Err("Trim edge must be start or end.".into());
    }
    let mut next = inner.project.clone();
    let clock = oc_dsl::timing::ProjectClock::new(&next.properties)?;
    let mut clip = next
        .audio_clip(clip_index)
        .ok_or_else(|| format!("Audio clip {clip_index} out of range"))?
        .clone();
    let (old_start_seconds, old_end_seconds) = clock.audio_clip_range_seconds(&clip)?;
    let root = inner.file_path.as_deref().and_then(std::path::Path::parent);
    freeze_clip_tempo_follow(&mut clip, old_end_seconds - old_start_seconds, root)?;
    let mut placement = clip.placement;
    if start != clip.time_range.start || edge == Some("start") {
        placement.start_offset_seconds = 0.0;
    }
    if end != clip.time_range.end || edge == Some("end") {
        placement.end_offset_seconds = 0.0;
    }
    let start_seconds =
        clock.seconds_at_quarter(clock.quarter_notes_at(&start)?)? + placement.start_offset_seconds;
    let end_seconds =
        clock.seconds_at_quarter(clock.quarter_notes_at(&end)?)? + placement.end_offset_seconds;
    if end_seconds <= start_seconds {
        return Err("Audio clip end must be after its start".into());
    }
    let sample_rate = inner.audio.audio_io.spec().sample_rate;
    let (old_start_frame, _) = clock.audio_clip_range_frames(&clip, sample_rate)?;
    let mut target = clip.clone();
    target.time_range = ast::TimeRange { start, end };
    target.placement = placement;
    let (new_start_frame, _) = clock.audio_clip_range_frames(&target, sample_rate)?;
    let delta = (new_start_frame as f64 - old_start_frame as f64) / f64::from(sample_rate);
    let clip_seconds = end_seconds - start_seconds;
    // Trimming the head skips the same span of media; extending it reveals media
    // until the file start, then holds silence.
    let mut offset = clip.offset_seconds + delta;
    let mut loop_seconds = clip.loop_seconds;
    if loop_seconds > 1e-9 {
        // The loop window starts at the clip's offset, so the head trim moves the
        // window with it and a trim past one loop wraps to the same media.
        offset = clip.offset_seconds + delta.rem_euclid(loop_seconds);
        let root = inner.file_path.as_deref().and_then(std::path::Path::parent);
        let content = oc_engine::audio_clip::content_seconds(&next, &clip, root)?;
        if loop_seconds >= clip_seconds - 1e-9 {
            // The trimmed clip repeats nowhere inside its length: one pass plays
            // the same media, and the engine refuses a loop the clip cannot fit.
            loop_seconds = 0.0;
            if offset + clip_seconds > content + 1e-9 {
                return Err(LOOP_PAST_MEDIA.into());
            }
        } else if offset + loop_seconds > content + 1e-9 {
            return Err(LOOP_PAST_MEDIA.into());
        }
    }
    let fade_in_seconds = clip.fade_in_seconds.min(clip_seconds);
    let fade_out_seconds = clip.fade_out_seconds.min(clip_seconds);
    {
        let adjusted = next.audio_clip_mut(clip_index).unwrap();
        adjusted.stretch = clip.stretch;
        adjusted.tempo_follow = clip.tempo_follow;
        let clip = adjusted;
        clip.offset_seconds = offset.max(0.0);
        clip.loop_seconds = loop_seconds;
        clip.fade_in_seconds = fade_in_seconds;
        clip.fade_out_seconds = fade_out_seconds;
        clip.gain_envelope = trim_gain_envelope(
            &clip.gain_envelope,
            old_start_seconds,
            old_end_seconds,
            start_seconds,
            end_seconds,
        );
        clip.time_range = ast::TimeRange { start, end };
        clip.placement = placement;
    }
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid audio clip trim: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(
        inner,
        old,
        UndoEdit::new("audio clip trim", UndoScope::Project),
    )?;
    Ok(())
}

/// Remap a relative envelope when the clip's time range is trimmed or extended.
///
/// Normalised positions follow the old clip inside the new one; the new head
/// and tail hold the edge values, so an extension adds a flat lead-in or tail
/// instead of stretching the curve.
fn trim_gain_envelope(
    points: &[GainEnvelopePoint],
    old_start: f64,
    old_end: f64,
    new_start: f64,
    new_end: f64,
) -> Vec<GainEnvelopePoint> {
    if points.is_empty() {
        return Vec::new();
    }
    let old_span = old_end - old_start;
    let new_span = new_end - new_start;
    if old_span <= 1e-9 || new_span <= 1e-9 {
        return points.to_vec();
    }
    let scale = old_span / new_span;
    let offset = (old_start - new_start) / new_span;
    let value_at_new = |position: f64| {
        let old_position = (position - offset) / scale;
        envelope_gain_at(points, old_position)
    };
    let mut next: Vec<GainEnvelopePoint> = vec![GainEnvelopePoint {
        position: 0.0,
        gain_db: value_at_new(0.0),
    }];
    for point in points {
        let position = offset + point.position * scale;
        if position > 1e-9 && position < 1.0 - 1e-9 {
            next.push(GainEnvelopePoint {
                position,
                gain_db: point.gain_db,
            });
        }
    }
    next.push(GainEnvelopePoint {
        position: 1.0,
        gain_db: value_at_new(1.0),
    });
    next.sort_by(|left, right| {
        left.position
            .partial_cmp(&right.position)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    next
}

/// Gain of a relative envelope at a normalised clip position.
fn envelope_gain_at(points: &[GainEnvelopePoint], position: f64) -> f64 {
    let Some(first) = points.first() else {
        return 0.0;
    };
    if position <= first.position {
        return first.gain_db;
    }
    for pair in points.windows(2) {
        if position <= pair[1].position {
            let span = pair[1].position - pair[0].position;
            if span <= 0.0 {
                return pair[1].gain_db;
            }
            return pair[0].gain_db
                + (pair[1].gain_db - pair[0].gain_db) * ((position - pair[0].position) / span);
        }
    }
    points.last().map_or(0.0, |point| point.gain_db)
}

/// Split a relative envelope at `ratio`, remapping both halves back to 0..1.
///
/// Each half gains an endpoint at the seam, so the two clips keep the same
/// curve when they are played back to back.
fn split_gain_envelope(
    points: &[GainEnvelopePoint],
    ratio: f64,
) -> (Vec<GainEnvelopePoint>, Vec<GainEnvelopePoint>) {
    if points.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let ratio = ratio.clamp(1e-6, 1.0 - 1e-6);
    let seam = envelope_gain_at(points, ratio);
    let mut left: Vec<GainEnvelopePoint> = points
        .iter()
        .filter(|point| point.position < ratio)
        .map(|point| GainEnvelopePoint {
            position: (point.position / ratio).clamp(0.0, 1.0),
            gain_db: point.gain_db,
        })
        .collect();
    left.push(GainEnvelopePoint {
        position: 1.0,
        gain_db: seam,
    });
    let mut right: Vec<GainEnvelopePoint> = vec![GainEnvelopePoint {
        position: 0.0,
        gain_db: seam,
    }];
    right.extend(
        points
            .iter()
            .filter(|point| point.position > ratio)
            .map(|point| GainEnvelopePoint {
                position: ((point.position - ratio) / (1.0 - ratio)).clamp(0.0, 1.0),
                gain_db: point.gain_db,
            }),
    );
    (left, right)
}

pub(crate) fn split_audio_clip_inner(
    inner: &mut crate::state::AppStateInner,
    clip_index: usize,
    at: ast::BarBeat,
) -> Result<(), String> {
    let mut next = inner.project.clone();
    let clock = oc_dsl::timing::ProjectClock::new(&next.properties)?;
    let root = inner.file_path.as_deref().and_then(std::path::Path::parent);
    split_audio_clip_at(
        &mut next,
        &clock,
        clip_index,
        at,
        root,
        inner.audio.audio_io.spec().sample_rate,
    )?;
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid audio clip split: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(
        inner,
        old,
        UndoEdit::new("audio clip split", UndoScope::Project),
    )?;
    Ok(())
}

// Keep the original baked media mapping when an edit changes clip boundaries.
fn freeze_clip_tempo_follow(
    clip: &mut ast::AudioClip,
    duration: f64,
    root: Option<&std::path::Path>,
) -> Result<(), String> {
    if clip.tempo_follow {
        if clip.stretch_mode != ast::StretchMode::Off {
            let source = oc_engine::audio_clip::source_seconds(clip, root)?;
            if source <= 0.0 {
                return Err("Cannot preserve the timing of an empty source.".into());
            }
            clip.stretch = duration / source;
        }
        clip.tempo_follow = false;
    }
    Ok(())
}

/// Split one clip in `project` at `at` without validating or recording undo.
///
/// Comping splits several clips in one gesture, so the caller owns validation
/// and the single history entry.
fn split_audio_clip_at(
    project: &mut ast::Project,
    clock: &oc_dsl::timing::ProjectClock,
    clip_index: usize,
    at: ast::BarBeat,
    root: Option<&std::path::Path>,
    sample_rate: u32,
) -> Result<(), String> {
    let split_quarter = clock.quarter_notes_at(&at)?;
    let mut clip = project
        .audio_clip(clip_index)
        .ok_or_else(|| format!("Audio clip {clip_index} out of range"))?
        .clone();
    let (start_seconds, end_seconds) = clock.audio_clip_range_seconds(&clip)?;
    let split_absolute = clock.seconds_at_quarter(split_quarter)?;
    if split_absolute <= start_seconds + 1e-9 || split_absolute >= end_seconds - 1e-9 {
        return Err("Split point must be inside the audio clip".into());
    }
    let (start_frame, end_frame) = clock.audio_clip_range_frames(&clip, sample_rate)?;
    let split_frame = clock.sample_at_quarter(split_quarter, sample_rate)?;
    if split_frame <= start_frame || split_frame >= end_frame {
        return Err("Split point must leave at least one rendered sample on each side.".into());
    }
    let split_seconds = (split_frame as f64 - start_frame as f64) / f64::from(sample_rate);
    let left_seconds = split_absolute - start_seconds;
    let right_seconds = end_seconds - split_absolute;
    freeze_clip_tempo_follow(&mut clip, left_seconds + right_seconds, root)?;
    // The loop repeats `[offset, offset + loop)` from the clip's start, so the
    // right half resumes at the media the seam leaves off at. A loop that
    // repeats nowhere inside a half plays one pass; a seam still inside the
    // right half has no expressible window and is refused.
    let mut right_offset = clip.offset_seconds + split_seconds;
    let mut left_loop = 0.0;
    let mut right_loop = 0.0;
    if clip.loop_seconds > 1e-9 {
        let loop_seconds = clip.loop_seconds;
        let wrapped = split_seconds.rem_euclid(loop_seconds);
        right_offset = clip.offset_seconds + wrapped;
        if loop_seconds < left_seconds - 1e-9 {
            left_loop = loop_seconds;
        }
        if loop_seconds < right_seconds - 1e-9 {
            right_loop = loop_seconds;
        }
        if right_loop == 0.0 {
            let to_seam = if wrapped > 1e-9 {
                loop_seconds - wrapped
            } else {
                loop_seconds
            };
            if right_seconds > to_seam + 1e-9 {
                return Err(SPLIT_AUDIO_LOOP_SEAM.into());
            }
        }
        if right_loop > 0.0 {
            let content = oc_engine::audio_clip::content_seconds(project, &clip, root)?;
            if right_offset + right_loop > content + 1e-9 {
                return Err(SPLIT_AUDIO_LOOP_PAST_MEDIA.into());
            }
        }
    }
    let split_ratio = if left_seconds + right_seconds > 1e-9 {
        left_seconds / (left_seconds + right_seconds)
    } else {
        0.5
    };
    let (left_envelope, right_envelope) = split_gain_envelope(&clip.gain_envelope, split_ratio);
    let name = unique_audio_clip_name(project, &clip.name);
    let second = ast::AudioClip {
        name,
        track: clip.track.clone(),
        file: clip.file.clone(),
        time_range: ast::TimeRange {
            start: at,
            end: clip.time_range.end,
        },
        placement: ast::AudioClipPlacement {
            start_offset_seconds: 0.0,
            end_offset_seconds: clip.placement.end_offset_seconds,
        },
        offset_seconds: right_offset,
        gain_db: clip.gain_db,
        fade_in_seconds: 0.0,
        fade_out_seconds: clip.fade_out_seconds.min(right_seconds),
        invert: clip.invert,
        crossfade_seconds: 0.0,
        stretch: clip.stretch,
        stretch_mode: clip.stretch_mode,
        tempo_follow: false,
        pitch: clip.pitch,
        formant: clip.formant,
        bypass_processing: clip.bypass_processing,
        preserve_formants: clip.preserve_formants,
        warp_markers: clip.warp_markers.clone(),
        loop_seconds: right_loop,
        take_group: clip.take_group.clone(),
        take: clip.take,
        muted: clip.muted,
        gain_envelope: right_envelope,
    };
    {
        let left = project.audio_clip_mut(clip_index).unwrap();
        left.stretch = clip.stretch;
        left.tempo_follow = clip.tempo_follow;
        let clip = left;
        clip.time_range.end = at;
        clip.placement.end_offset_seconds = 0.0;
        clip.gain_envelope = left_envelope;
        clip.fade_in_seconds = clip.fade_in_seconds.min(left_seconds);
        clip.fade_out_seconds = 0.0;
        clip.crossfade_seconds = 0.0;
        clip.loop_seconds = left_loop;
    }
    project
        .insert_audio_clip(clip_index + 1, second)
        .map_err(|error| format!("Invalid audio clip split: {error}"))?;
    Ok(())
}

/// The take group the comp gesture works on: the group of a `take` clip of
/// `track` that covers the range. A track holds one recording group at a time.
fn comp_group_for(
    project: &ast::Project,
    clock: &ProjectClock,
    track: &str,
    take: u32,
    start: f64,
    end: f64,
) -> Result<String, String> {
    let mut groups = BTreeSet::new();
    for clip in project.audio_clips() {
        if clip.track != track || clip.take != Some(take) {
            continue;
        }
        let Some(group) = &clip.take_group else {
            continue;
        };
        let (clip_start, clip_end) = clock.audio_clip_range_seconds(clip)?;
        let clip_start = clock.quarter_at_seconds(clip_start)?;
        let clip_end = clock.quarter_at_seconds(clip_end)?;
        if clip_end > start + 1e-9 && clip_start < end - 1e-9 {
            groups.insert(group.clone());
        }
    }
    match groups.len() {
        0 => Err(format!(
            "No take {take} on track '{track}' covers this range."
        )),
        1 => Ok(groups.into_iter().next().unwrap()),
        _ => Err(format!(
            "Track '{track}' has more than one take group over this range; comp one session at a time."
        )),
    }
}

/// Comp one group: split the chosen take at both edges and mute every other
/// take over the range. The whole gesture is one history entry.
pub(crate) fn comp_audio_takes_inner(
    inner: &mut crate::state::AppStateInner,
    track: &str,
    take: u32,
    start: ast::BarBeat,
    end: ast::BarBeat,
) -> Result<(), String> {
    if take == 0 {
        return Err("Take numbers start at 1.".into());
    }
    let mut next = inner.project.clone();
    if !next.tracks.iter().any(|candidate| candidate.name == track) {
        return Err(format!("Track '{track}' does not exist."));
    }
    let clock = ProjectClock::new(&next.properties)?;
    let start_quarter = clock.quarter_notes_at(&start)?;
    let end_quarter = clock.quarter_notes_at(&end)?;
    if end_quarter <= start_quarter + 1e-9 {
        return Err("Comp range must end after it starts.".into());
    }
    let group = comp_group_for(&next, &clock, track, take, start_quarter, end_quarter)?;
    let mut indices: Vec<usize> = next
        .audio_clips()
        .iter()
        .enumerate()
        .filter(|(_, clip)| {
            clip.track == track && clip.take_group.as_deref() == Some(group.as_str())
        })
        .map(|(index, _)| index)
        .collect();
    // Splitting inserts after the clip, so process from the end: indices of
    // clips not handled yet never shift.
    indices.sort_unstable_by(|a, b| b.cmp(a));
    let root = inner.file_path.as_deref().and_then(std::path::Path::parent);
    for index in indices {
        let (clip_start, clip_end) = {
            let clip = next.audio_clip(index).unwrap();
            (
                clock.quarter_at_seconds(clock.audio_clip_range_seconds(clip)?.0)?,
                clock.quarter_at_seconds(clock.audio_clip_range_seconds(clip)?.1)?,
            )
        };
        if clip_end <= start_quarter + 1e-9 || clip_start >= end_quarter - 1e-9 {
            continue;
        }
        let mut middle = index;
        if clip_start < start_quarter - 1e-9 {
            split_audio_clip_at(
                &mut next,
                &clock,
                index,
                start,
                root,
                inner.audio.audio_io.spec().sample_rate,
            )?;
            middle = index + 1;
        }
        let middle_end = clock.quarter_at_seconds(
            clock
                .audio_clip_range_seconds(next.audio_clip(middle).unwrap())?
                .1,
        )?;
        if end_quarter < middle_end - 1e-9 {
            split_audio_clip_at(
                &mut next,
                &clock,
                middle,
                end,
                root,
                inner.audio.audio_io.spec().sample_rate,
            )?;
        }
        let chosen = next.audio_clip_mut(middle).unwrap().take == Some(take);
        next.audio_clip_mut(middle).unwrap().muted = !chosen;
    }
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid comp: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(inner, old, UndoEdit::new("comp takes", UndoScope::Project))?;
    Ok(())
}

/// Mute or unmute every clip of one take as a single document edit.
pub(crate) fn set_audio_take_mute_inner(
    inner: &mut crate::state::AppStateInner,
    track: &str,
    take: u32,
    muted: bool,
) -> Result<(), String> {
    if take == 0 {
        return Err("Take numbers start at 1.".into());
    }
    let mut next = inner.project.clone();
    let mut found = false;
    for clip in next.audio_clips_mut() {
        if clip.track == track && clip.take == Some(take) {
            clip.muted = muted;
            found = true;
        }
    }
    if !found {
        return Err(format!("Track '{track}' has no take {take}."));
    }
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid take mute: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    let label = if muted {
        format!("mute take {take}")
    } else {
        format!("unmute take {take}")
    };
    resync_with_undo(inner, old, UndoEdit::new(&label, UndoScope::Project))?;
    Ok(())
}

/// Clip gain range the timeline accepts, in dB. A clip quieter than the floor
/// is silent anyway; above the ceiling the envelope would only clip the bus.
pub const CLIP_GAIN_MIN_DB: f64 = -60.0;
pub const CLIP_GAIN_MAX_DB: f64 = 24.0;

/// Shortest fade the timeline accepts. A shorter ramp clicks instead of fading.
pub const MIN_FADE_SECONDS: f64 = 0.020;

/// Refusals the timeline repeats verbatim in its live readout.
pub const FADE_TOO_SHORT: &str = "A fade needs at least 20 ms.";
pub const CLIP_HAS_NO_MEDIA: &str =
    "This clip has no audio media, so its gain and fades cannot be edited.";

/// Set any part of an audio clip's gain/fade envelope in one undoable edit.
///
/// A drag gesture changes one field, so every field is optional and only the
/// ones present are written; the clip's other envelope values stay untouched.
#[expect(
    clippy::too_many_arguments,
    reason = "Tauri preserves the flat IPC argument keys consumed by existing clients"
)]
#[tauri::command]
pub fn set_audio_clip_envelope(
    clip_index: usize,
    gain_db: Option<f64>,
    fade_in_seconds: Option<f64>,
    fade_out_seconds: Option<f64>,
    crossfade_seconds: Option<f64>,
    gain_envelope: Option<Vec<GainEnvelopePoint>>,
    expected_revision: Option<u64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    guard_gui_clip_edit(&inner, expected_revision)?;
    set_audio_clip_envelope_full(
        &mut inner,
        clip_index,
        gain_db,
        fade_in_seconds,
        fade_out_seconds,
        crossfade_seconds,
        gain_envelope,
    )?;
    Ok(snapshot(&inner))
}

/// The legacy five-field entry point: gain and fades only, no crossfade or
/// point envelope. The timeline command uses [`set_audio_clip_envelope_full`].
#[cfg(test)]
pub(crate) fn set_audio_clip_envelope_inner(
    inner: &mut crate::state::AppStateInner,
    clip_index: usize,
    gain_db: Option<f64>,
    fade_in_seconds: Option<f64>,
    fade_out_seconds: Option<f64>,
) -> Result<(), String> {
    set_audio_clip_envelope_full(
        inner,
        clip_index,
        gain_db,
        fade_in_seconds,
        fade_out_seconds,
        None,
        None,
    )
}

/// The full envelope edit: the static gain, the fades, the crossfade and the
/// optional point envelope. Only the arguments that are present are written.
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_audio_clip_envelope_full(
    inner: &mut crate::state::AppStateInner,
    clip_index: usize,
    gain_db: Option<f64>,
    fade_in_seconds: Option<f64>,
    fade_out_seconds: Option<f64>,
    crossfade_seconds: Option<f64>,
    gain_envelope: Option<Vec<GainEnvelopePoint>>,
) -> Result<(), String> {
    if gain_db.is_none()
        && fade_in_seconds.is_none()
        && fade_out_seconds.is_none()
        && crossfade_seconds.is_none()
        && gain_envelope.is_none()
    {
        return Ok(());
    }
    let mut next = inner.project.clone();
    let clock = ProjectClock::new(&next.properties)?;
    let (file, clip_seconds) = {
        let clip = next
            .audio_clip(clip_index)
            .ok_or_else(|| format!("Audio clip {clip_index} out of range"))?;
        let (start, end) = clock.audio_clip_range_seconds(clip)?;
        let seconds = end - start;
        (clip.file.clone(), seconds.max(0.0))
    };
    ensure_clip_media(inner, &file)?;
    let label = envelope_label(
        gain_db,
        fade_in_seconds,
        fade_out_seconds,
        crossfade_seconds,
        gain_envelope.is_some(),
    );
    let before = next.audio_clip(clip_index).unwrap().clone();
    {
        let clip = next.audio_clip_mut(clip_index).unwrap();
        if let Some(gain) = gain_db {
            if !gain.is_finite() {
                return Err("Clip gain must be a finite number of decibels.".into());
            }
            clip.gain_db = gain.clamp(CLIP_GAIN_MIN_DB, CLIP_GAIN_MAX_DB);
        }
        if let Some(fade) = fade_in_seconds {
            clip.fade_in_seconds = clip_fade(fade, clip_seconds)?;
        }
        if let Some(fade) = fade_out_seconds {
            clip.fade_out_seconds = clip_fade(fade, clip_seconds)?;
        }
        if let Some(crossfade) = crossfade_seconds {
            clip.crossfade_seconds = clip_fade(crossfade, clip_seconds)?;
        }
        if let Some(points) = gain_envelope {
            clip.gain_envelope = checked_gain_envelope(points)?;
        }
    }
    if *next.audio_clip_mut(clip_index).unwrap() == before {
        return Ok(());
    }
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid clip envelope: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(inner, old, UndoEdit::new(label, UndoScope::Project))?;
    Ok(())
}

/// Reject non-finite envelope values and clamp each gain to the clip range.
///
/// Positions are left to the DSL validator, which also checks their order.
fn checked_gain_envelope(points: Vec<GainEnvelopePoint>) -> Result<Vec<GainEnvelopePoint>, String> {
    let mut checked = Vec::with_capacity(points.len());
    for point in points {
        if !point.position.is_finite() {
            return Err(format!(
                "Gain envelope position must be finite, got {}.",
                point.position
            ));
        }
        if !point.gain_db.is_finite() {
            return Err(format!(
                "Gain envelope gain must be finite decibels, got {}.",
                point.gain_db
            ));
        }
        checked.push(GainEnvelopePoint {
            position: point.position,
            gain_db: point.gain_db.clamp(CLIP_GAIN_MIN_DB, CLIP_GAIN_MAX_DB),
        });
    }
    Ok(checked)
}

/// A fade past the clip end clamps at the length; a ramp shorter than the
/// click threshold is refused rather than written.
fn clip_fade(seconds: f64, clip_seconds: f64) -> Result<f64, String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("A fade must be zero or longer.".into());
    }
    let clamped = seconds.min(clip_seconds);
    if clamped > 0.0 && clamped < MIN_FADE_SECONDS {
        return Err(FADE_TOO_SHORT.into());
    }
    Ok(clamped)
}

/// The clip's media must exist: an absent waveform has no ramp to draw.
fn ensure_clip_media(inner: &crate::state::AppStateInner, file: &str) -> Result<(), String> {
    let root = inner.file_path.as_deref().and_then(std::path::Path::parent);
    match oc_engine::audio_clip::resolve_path(file, root) {
        Ok(path) if path.is_file() => Ok(()),
        _ => Err(CLIP_HAS_NO_MEDIA.into()),
    }
}

fn envelope_label(
    gain_db: Option<f64>,
    fade_in_seconds: Option<f64>,
    fade_out_seconds: Option<f64>,
    crossfade_seconds: Option<f64>,
    gain_envelope: bool,
) -> &'static str {
    match (
        gain_db.is_some(),
        fade_in_seconds.is_some(),
        fade_out_seconds.is_some(),
        crossfade_seconds.is_some(),
        gain_envelope,
    ) {
        (true, false, false, false, false) => "clip gain",
        (false, true, false, false, false) => "fade in",
        (false, false, true, false, false) => "fade out",
        (false, false, false, true, false) => "crossfade",
        (false, false, false, false, true) => "gain envelope",
        _ => "clip envelope",
    }
}

pub(crate) fn remove_audio_clip_inner(
    inner: &mut crate::state::AppStateInner,
    clip_index: usize,
) -> Result<(), String> {
    let mut next = inner.project.clone();
    if clip_index >= next.audio_clip_count() {
        return Err(format!("Audio clip {clip_index} out of range"));
    }
    next.remove_audio_clip(clip_index);
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(
        inner,
        old,
        UndoEdit::new("audio clip remove", UndoScope::Project),
    )?;
    Ok(())
}

fn unique_audio_clip_name(project: &ast::Project, base: &str) -> String {
    for suffix in 2.. {
        let candidate = format!("{base} {suffix}");
        if !project
            .audio_clips()
            .iter()
            .any(|clip| clip.name == candidate)
        {
            return candidate;
        }
    }
    unreachable!("the suffix loop always returns")
}

// ── Slip: move content under a fixed clip ─────────────────────────────────────

/// Refusals the timeline repeats verbatim in its live readout.
pub const SLIP_NOTHING_TO_SLIP: &str = "Nothing to slip: this clip already shows the whole file.";
pub const SLIP_NOTHING_TO_SLIP_PATTERN: &str =
    "Nothing to slip: this clip already shows the whole pattern.";

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioClipPlayback {
    pub stretch: Option<f64>,
    pub pitch: Option<f64>,
    pub formant: Option<f64>,
    pub preserve_formants: Option<bool>,
    pub bypass_processing: Option<bool>,
    pub warp_markers: Option<Vec<ast::AudioWarpMarker>>,
    pub stretch_to: Option<ast::BarBeat>,
    pub stretch_mode: Option<oc_dsl::ast::StretchMode>,
    pub tempo_follow: Option<bool>,
}

#[tauri::command]
pub async fn set_audio_clip_playback(
    clip_index: usize,
    playback: AudioClipPlayback,
    expected_revision: Option<u64>,
    state: State<'_, AppState>,
) -> Result<ProjectSnapshot, String> {
    let edit = {
        let inner = state.lock_inner();
        guard_gui_clip_edit(&inner, expected_revision)?;
        ClipPlaybackEdit::capture(&inner)
    };
    let edit = tauri::async_runtime::spawn_blocking(move || edit.prepare(clip_index, playback))
        .await
        .map_err(|error| format!("Clip playback validation failed: {error}"))??;
    let mut inner = state.lock_inner();
    guard_gui_clip_edit(&inner, expected_revision)?;
    edit.commit(&mut inner)?;
    Ok(snapshot(&inner))
}

pub(super) fn guard_gui_clip_edit(
    inner: &crate::state::AppStateInner,
    expected_revision: Option<u64>,
) -> Result<(), String> {
    if inner.transaction.is_some() {
        return Err("Wait for the active agent transaction before editing the clip.".into());
    }
    inner.check_revision(expected_revision)
}

pub(crate) struct ClipPlaybackEdit {
    project: ast::Project,
    file_path: Option<std::path::PathBuf>,
    revision: u64,
    epoch: u64,
    label: Option<&'static str>,
}

impl ClipPlaybackEdit {
    pub(crate) fn capture(inner: &crate::state::AppStateInner) -> Self {
        Self {
            project: inner.project.clone(),
            file_path: inner.file_path.clone(),
            revision: inner.revision,
            epoch: inner.content_epoch,
            label: None,
        }
    }

    pub(crate) fn prepare(
        mut self,
        clip_index: usize,
        playback: AudioClipPlayback,
    ) -> Result<Self, String> {
        if playback.stretch.is_some() && playback.stretch_to.is_some() {
            return Err("Choose a duration ratio or a stretch end, not both.".into());
        }
        let fit_source = playback.tempo_follow == Some(true);
        if fit_source && playback.stretch.is_some() {
            return Err(
                "Choose a duration ratio or fit the whole source to the clip, not both.".into(),
            );
        }
        let original = self
            .project
            .audio_clip(clip_index)
            .ok_or_else(|| format!("Audio clip {clip_index} out of range"))?;
        let previous_markers = original.warp_markers.clone();
        let mut clip = original.clone();
        clip.stretch = playback.stretch.unwrap_or(clip.stretch);
        clip.pitch = playback.pitch.unwrap_or(clip.pitch);
        clip.formant = playback.formant.unwrap_or(clip.formant);
        clip.preserve_formants = playback.preserve_formants.unwrap_or(clip.preserve_formants);
        clip.bypass_processing = playback.bypass_processing.unwrap_or(clip.bypass_processing);
        clip.stretch_mode = playback.stretch_mode.unwrap_or(clip.stretch_mode);
        clip.tempo_follow = playback.tempo_follow.unwrap_or(clip.tempo_follow);
        if let Some(markers) = playback.warp_markers {
            clip.warp_markers = markers;
        }
        if fit_source {
            if clip.stretch_mode == ast::StretchMode::Off {
                return Err("Fit source to clip length requires a processing mode.".into());
            }
            // Fit uses the whole file; post-stretch trims or loops would select
            // another region. Unrelated edits retain authored legacy offsets.
            clip.offset_seconds = 0.0;
            clip.loop_seconds = 0.0;
        }
        if let Some(end) = playback.stretch_to {
            let clock = ProjectClock::new(&self.project.properties)?;
            let seconds = |at| clock.seconds_at_quarter(clock.quarter_notes_at(&at)?);
            let (start_seconds, old_end_seconds) = clock.audio_clip_range_seconds(&clip)?;
            let old_duration = old_end_seconds - start_seconds;
            let duration = seconds(end)? - start_seconds;
            if duration <= 0.0 || old_duration <= 0.0 {
                return Err("Stretch needs an end after the clip start.".into());
            }
            let factor = duration / old_duration;
            if clip.stretch_mode == ast::StretchMode::Off {
                clip.stretch_mode = ast::StretchMode::Musical;
                clip.stretch = 1.0;
                clip.tempo_follow = false;
            }
            if !clip.tempo_follow {
                clip.stretch *= factor;
            }
            for marker in &mut clip.warp_markers {
                marker.target_seconds *= factor;
            }
            clip.offset_seconds *= factor;
            clip.loop_seconds *= factor;
            clip.fade_in_seconds *= factor;
            clip.fade_out_seconds *= factor;
            clip.crossfade_seconds *= factor;
            clip.time_range.end = end;
            clip.placement.end_offset_seconds = 0.0;
        }
        if &clip == original {
            return Ok(self);
        }
        use oc_engine::stretch::{MAX_RATIO, MIN_RATIO};
        if (playback.stretch.is_some() || playback.stretch_to.is_some())
            && !(MIN_RATIO..=MAX_RATIO).contains(&clip.stretch)
        {
            return Err(format!(
                "Clip duration ratio must be between {MIN_RATIO} and {MAX_RATIO}."
            ));
        }

        *self.project.audio_clip_mut(clip_index).unwrap() = clip;
        oc_dsl::validate(&self.project)
            .map_err(|errors| format!("Invalid clip playback: {errors:?}"))?;
        let clip = self.project.audio_clip(clip_index).unwrap();
        let root = self.file_path.as_deref().and_then(std::path::Path::parent);
        // Validate the retained processing even while the renderer uses raw media.
        let mut configured = clip.clone();
        configured.bypass_processing = false;
        let content = oc_engine::audio_clip::content_seconds(&self.project, &configured, root)?;
        if clip.loop_seconds > 0.0 {
            if clip.loop_seconds < MIN_LOOP_SECONDS {
                return Err(LOOP_TOO_SHORT.into());
            }
            if clip.offset_seconds + clip.loop_seconds > content + 1e-9 {
                return Err(LOOP_PAST_MEDIA.into());
            }
        }
        self.label = Some(if playback.stretch_to.is_some() {
            "stretch audio clip"
        } else if clip.warp_markers != previous_markers {
            "edit audio warp markers"
        } else {
            "clip speed and pitch"
        });
        Ok(self)
    }

    pub(crate) fn commit(self, inner: &mut crate::state::AppStateInner) -> Result<(), String> {
        if inner.revision != self.revision
            || inner.content_epoch != self.epoch
            || inner.file_path != self.file_path
        {
            return Err(
                "The project changed while validating clip playback. Retry the edit.".into(),
            );
        }
        if let Some(label) = self.label {
            let old = std::mem::replace(&mut inner.project, self.project);
            resync_with_undo(inner, old, UndoEdit::new(label, UndoScope::Project))?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn set_audio_clip_playback_inner(
    inner: &mut crate::state::AppStateInner,
    clip_index: usize,
    playback: AudioClipPlayback,
) -> Result<(), String> {
    ClipPlaybackEdit::capture(inner)
        .prepare(clip_index, playback)?
        .commit(inner)
}

/// Hide `offset_seconds` of the clip's media head without moving the clip.
///
/// The offset is clamped to the media that is not already visible, so a slip
/// never leaves the clip's tail silent. A drag writes one value on release.
#[tauri::command]
pub fn set_audio_clip_offset(
    clip_index: usize,
    offset_seconds: f64,
    expected_revision: Option<u64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    guard_gui_clip_edit(&inner, expected_revision)?;
    set_audio_clip_offset_inner(&mut inner, clip_index, offset_seconds)?;
    Ok(snapshot(&inner))
}

pub(crate) fn set_audio_clip_offset_inner(
    inner: &mut crate::state::AppStateInner,
    clip_index: usize,
    offset_seconds: f64,
) -> Result<(), String> {
    if !offset_seconds.is_finite() || offset_seconds < 0.0 {
        return Err("A slip offset must be zero or longer.".into());
    }
    let mut next = inner.project.clone();
    let clock = ProjectClock::new(&next.properties)?;
    let clip = next
        .audio_clip(clip_index)
        .ok_or_else(|| format!("Audio clip {clip_index} out of range"))?
        .clone();
    let (start, end) = clock.audio_clip_range_seconds(&clip)?;
    let clip_seconds = end - start;
    ensure_clip_media(inner, &clip.file)?;
    let root = inner.file_path.as_deref().and_then(std::path::Path::parent);
    let content = oc_engine::audio_clip::content_seconds(&next, &clip, root)?;
    let ceiling = (content - clip_seconds).max(clip.offset_seconds).max(0.0);
    if ceiling <= 0.0 {
        return Err(SLIP_NOTHING_TO_SLIP.into());
    }
    let offset = offset_seconds.min(ceiling);
    if (offset - clip.offset_seconds).abs() <= 1e-9 {
        return Ok(());
    }
    next.audio_clip_mut(clip_index).unwrap().offset_seconds = offset;
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid clip slip: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(inner, old, UndoEdit::new("clip slip", UndoScope::Project))?;
    Ok(())
}

/// Hide `offset_quarters` of a pattern clip's content head without moving it.
#[tauri::command]
pub fn set_pattern_clip_offset(
    track_index: usize,
    clip_index: usize,
    offset_quarters: f64,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_pattern_clip_offset_inner(&mut inner, track_index, clip_index, offset_quarters)?;
    Ok(snapshot(&inner))
}

pub(crate) fn set_pattern_clip_offset_inner(
    inner: &mut crate::state::AppStateInner,
    track_index: usize,
    clip_index: usize,
    offset_quarters: f64,
) -> Result<(), String> {
    if !offset_quarters.is_finite() || offset_quarters < 0.0 {
        return Err("A slip offset must be zero or longer.".into());
    }
    let mut next = inner.project.clone();
    let clock = ProjectClock::new(&next.properties)?;
    let clip = next
        .tracks
        .get(track_index)
        .and_then(|track| track.pattern_clip(clip_index))
        .ok_or_else(|| format!("Clip {clip_index} on track {track_index} out of range"))?
        .clone();
    let start = clock.quarter_notes_at(&clip.time_range.start)?;
    let end = clock.quarter_notes_at(&clip.time_range.end)?;
    let range = (end - start).max(0.0);
    let content = next
        .patterns
        .iter()
        .find(|pattern| pattern.name == clip.pattern)
        .map(|pattern| pattern.content_quarters())
        .unwrap_or(0.0);
    let ceiling = (content - range).max(clip.offset_quarters).max(0.0);
    if ceiling <= 1e-9 {
        return Err(SLIP_NOTHING_TO_SLIP_PATTERN.into());
    }
    let offset = offset_quarters.min(ceiling);
    if (offset - clip.offset_quarters).abs() <= 1e-9 {
        return Ok(());
    }
    next.tracks[track_index]
        .pattern_clip_mut(clip_index)
        .unwrap()
        .offset_quarters = offset;
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid clip slip: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(inner, old, UndoEdit::new("clip slip", UndoScope::Project))?;
    Ok(())
}

// ── Clip loop points ─────────────────────────────────────────────────────────

/// Shortest loop the timeline accepts, from the engine's own floor: a shorter
/// region stutters instead of repeating.
pub const MIN_LOOP_SECONDS: f64 = oc_engine::audio_clip::MIN_LOOP_SECONDS;

/// Shortest pattern loop the timeline accepts, in quarter notes: a 16th.
pub const MIN_LOOP_QUARTERS: f64 = 0.25;

/// Refusals the timeline repeats verbatim in its live readout.
pub const LOOP_TOO_SHORT: &str = "A loop needs at least 100 ms.";
pub const LOOP_TOO_SHORT_MEDIA: &str = "Too short to loop: this file is shorter than 100 ms.";
pub const LOOP_NOT_SHORTER_THAN_CLIP: &str = "A loop must be shorter than the clip.";
pub const LOOP_PAST_MEDIA: &str = "The loop must end inside the media.";
pub const LOOP_TOO_SHORT_PATTERN: &str = "A loop needs at least a 16th note.";
pub const LOOP_PAST_CONTENT: &str = "The loop must end inside the pattern's content.";

/// Set or clear the loop a clip repeats. `None` restores a single pass.
#[tauri::command]
pub fn set_audio_clip_loop(
    clip_index: usize,
    loop_seconds: Option<f64>,
    expected_revision: Option<u64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    guard_gui_clip_edit(&inner, expected_revision)?;
    set_audio_clip_loop_inner(&mut inner, clip_index, loop_seconds)?;
    Ok(snapshot(&inner))
}

pub(crate) fn set_audio_clip_loop_inner(
    inner: &mut crate::state::AppStateInner,
    clip_index: usize,
    loop_seconds: Option<f64>,
) -> Result<(), String> {
    let mut next = inner.project.clone();
    let clock = ProjectClock::new(&next.properties)?;
    let clip = next
        .audio_clip(clip_index)
        .ok_or_else(|| format!("Audio clip {clip_index} out of range"))?
        .clone();
    let loop_seconds = loop_seconds.unwrap_or(0.0);
    if !loop_seconds.is_finite() || loop_seconds < 0.0 {
        return Err("A loop length must be zero or longer.".into());
    }
    if loop_seconds > 0.0 {
        // The media's playable length bounds the region the loop may repeat.
        ensure_clip_media(inner, &clip.file)?;
        let root = inner.file_path.as_deref().and_then(std::path::Path::parent);
        let content = oc_engine::audio_clip::content_seconds(&next, &clip, root)?;
        if content < MIN_LOOP_SECONDS {
            return Err(LOOP_TOO_SHORT_MEDIA.into());
        }
        if loop_seconds < MIN_LOOP_SECONDS {
            return Err(LOOP_TOO_SHORT.into());
        }
        let (start, end) = clock.audio_clip_range_seconds(&clip)?;
        let clip_seconds = end - start;
        if loop_seconds >= clip_seconds - 1e-9 {
            return Err(LOOP_NOT_SHORTER_THAN_CLIP.into());
        }
        if clip.offset_seconds + loop_seconds > content + 1e-9 {
            return Err(LOOP_PAST_MEDIA.into());
        }
    }
    if (loop_seconds - clip.loop_seconds).abs() <= 1e-9 {
        return Ok(());
    }
    next.audio_clip_mut(clip_index).unwrap().loop_seconds = loop_seconds;
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid clip loop: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(inner, old, UndoEdit::new("clip loop", UndoScope::Project))?;
    Ok(())
}

/// Written events can extend past the declared length. An explicitly declared
/// tail can also carry silence up to the next repeat.
fn pattern_loop_end_quarters(pattern: &ast::PatternDef) -> f64 {
    let written = pattern.content_quarters();
    if pattern.bars.is_some() {
        written.max(pattern.length_quarters())
    } else {
        written
    }
}

/// Set or clear the loop a pattern clip repeats. `None` restores a single pass.
#[tauri::command]
pub fn set_pattern_clip_loop(
    track_index: usize,
    clip_index: usize,
    loop_quarters: Option<f64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_pattern_clip_loop_inner(&mut inner, track_index, clip_index, loop_quarters)?;
    Ok(snapshot(&inner))
}

pub(crate) fn set_pattern_clip_loop_inner(
    inner: &mut crate::state::AppStateInner,
    track_index: usize,
    clip_index: usize,
    loop_quarters: Option<f64>,
) -> Result<(), String> {
    let mut next = inner.project.clone();
    let clock = ProjectClock::new(&next.properties)?;
    let clip = next
        .tracks
        .get(track_index)
        .and_then(|track| track.pattern_clip(clip_index))
        .ok_or_else(|| format!("Clip {clip_index} on track {track_index} out of range"))?
        .clone();
    let loop_quarters = loop_quarters.unwrap_or(0.0);
    if !loop_quarters.is_finite() || loop_quarters < 0.0 {
        return Err("A loop length must be zero or longer.".into());
    }
    if loop_quarters > 0.0 {
        let start = clock.quarter_notes_at(&clip.time_range.start)?;
        let end = clock.quarter_notes_at(&clip.time_range.end)?;
        let range = (end - start).max(0.0);
        let content = next
            .patterns
            .iter()
            .find(|pattern| pattern.name == clip.pattern)
            .map(pattern_loop_end_quarters)
            .unwrap_or(0.0);
        if loop_quarters < MIN_LOOP_QUARTERS {
            return Err(LOOP_TOO_SHORT_PATTERN.into());
        }
        if loop_quarters >= range - 1e-9 {
            return Err(LOOP_NOT_SHORTER_THAN_CLIP.into());
        }
        if clip.offset_quarters + loop_quarters > content + 1e-9 {
            return Err(LOOP_PAST_CONTENT.into());
        }
    }
    if (loop_quarters - clip.loop_quarters).abs() <= 1e-9 {
        return Ok(());
    }
    next.tracks[track_index]
        .pattern_clip_mut(clip_index)
        .unwrap()
        .loop_quarters = loop_quarters;
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid clip loop: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(inner, old, UndoEdit::new("clip loop", UndoScope::Project))?;
    Ok(())
}
