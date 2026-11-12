//! Clip groups: grouping a timeline selection, breaking a group, and moving a
//! group as one unit.
//!
//! Membership is by clip identifier, so a group survives a move and a project
//! without groups writes the same DSL bytes it wrote before grouping.

use oc_dsl::ast;
use oc_dsl::groups;
use tauri::State;

use crate::state::{AppState, UndoEdit, UndoScope};

use super::shared::{ProjectSnapshot, resync_with_undo, snapshot};

/// One named clip addressed inside a track's clip list.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipRef {
    pub track_index: usize,
    pub clip_index: usize,
}

fn pairs(clips: &[ClipRef]) -> Vec<(usize, usize)> {
    clips
        .iter()
        .map(|clip| (clip.track_index, clip.clip_index))
        .collect()
}

fn validate(project: &ast::Project, failed: &str) -> Result<(), String> {
    oc_dsl::validate(project).map_err(|errors| {
        let text = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        format!("{failed}: {text}")
    })
}

/// Group the named clips into one new group. Returns the group identifier.
pub(crate) fn apply_group(project: &mut ast::Project, clips: &[ClipRef]) -> Result<String, String> {
    let mut candidate = project.clone();
    let id = groups::group_clips(&mut candidate, &pairs(clips))?;
    validate(&candidate, &format!("group '{id}' was not saved"))?;
    *project = candidate;
    Ok(id)
}

/// Break every group the named clips belong to. Returns the removed group ids.
pub(crate) fn apply_ungroup(
    project: &mut ast::Project,
    clips: &[ClipRef],
) -> Result<Vec<String>, String> {
    let mut candidate = project.clone();
    let mut removed = Vec::new();
    for (track_index, clip_index) in pairs(clips) {
        let Some(id) = candidate
            .tracks
            .get(track_index)
            .and_then(|track| track.pattern_clip(clip_index))
            .and_then(|clip| clip.id.clone())
        else {
            continue;
        };
        if groups::group_of(&candidate, &id).is_none() {
            continue;
        }
        removed.push(groups::ungroup_clips(&mut candidate, &[id])?);
    }
    if removed.is_empty() {
        return Err("none of the selected clips belongs to a group".into());
    }
    validate(
        &candidate,
        &format!("group '{}' was not removed", removed.join("', '")),
    )?;
    *project = candidate;
    Ok(removed)
}

/// Move every member of the anchor clip's group by the anchor's own delta and
/// keep each member on its track. Refuses before any member crosses bar 1.
pub(crate) fn apply_move_clip_group(
    project: &mut ast::Project,
    track_index: usize,
    clip_index: usize,
    start: ast::BarBeat,
) -> Result<(), String> {
    let mut candidate = project.clone();
    let clock = oc_dsl::timing::ProjectClock::new(&candidate.properties)?;
    let anchor = candidate
        .tracks
        .get(track_index)
        .and_then(|track| track.pattern_clip(clip_index))
        .ok_or_else(|| format!("clip {clip_index} on track {track_index} is not in the project"))?;
    let id = anchor
        .id
        .clone()
        .ok_or_else(|| "this clip is not in a group".to_string())?;
    let group = groups::group_of(&candidate, &id)
        .ok_or_else(|| "this clip is not in a group".to_string())?;
    let members: Vec<(usize, usize)> = group
        .members
        .iter()
        .filter_map(|member| groups::clip_by_id(&candidate, member))
        .map(|(track, index, _)| (track, index))
        .collect();

    let anchor_start = clock.quarter_notes_at(&anchor.time_range.start)?;
    let target = clock.quarter_notes_at(&start)?;
    let delta = target - anchor_start;
    if delta.abs() < 1e-12 {
        return Ok(());
    }

    let mut moved = Vec::with_capacity(members.len());
    for (track, index) in members {
        let clip = &candidate.tracks[track].pattern_clip(index).unwrap();
        let start = clock.quarter_notes_at(&clip.time_range.start)?;
        let end = clock.quarter_notes_at(&clip.time_range.end)?;
        if start + delta < -1e-9 {
            return Err(format!("clip '{}' would start before bar 1", clip.pattern));
        }
        moved.push((track, index, start + delta, end + delta));
    }
    for (track, index, start, end) in moved {
        candidate.tracks[track]
            .pattern_clip_mut(index)
            .unwrap()
            .time_range = ast::TimeRange {
            start: clock.bar_beat_at_quarter(start)?,
            end: clock.bar_beat_at_quarter(end)?,
        };
    }
    validate(&candidate, "the group was not moved")?;
    *project = candidate;
    Ok(())
}

/// Group clips into one move.
#[tauri::command]
pub fn group_clips(clips: Vec<ClipRef>, state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old = inner.project.clone();
    apply_group(&mut inner.project, &clips)?;
    resync_with_undo(
        &mut inner,
        old,
        UndoEdit::new("group clips", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Ungroup clips.
#[tauri::command]
pub fn ungroup_clips(
    clips: Vec<ClipRef>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old = inner.project.clone();
    apply_ungroup(&mut inner.project, &clips)?;
    resync_with_undo(
        &mut inner,
        old,
        UndoEdit::new("ungroup clips", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Move a clip group.
#[tauri::command]
pub fn move_clip_group(
    track_index: usize,
    clip_index: usize,
    start_bar: u32,
    start_beat: u32,
    start_sub: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old = inner.project.clone();
    apply_move_clip_group(
        &mut inner.project,
        track_index,
        clip_index,
        ast::BarBeat {
            bar: start_bar,
            beat: start_beat,
            subdivision: start_sub,
        },
    )?;
    resync_with_undo(
        &mut inner,
        old,
        UndoEdit::new("clip move", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}
