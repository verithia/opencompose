//! Timeline gestures on automation clips: move, resize, duplicate, split, delete.
//!
//! A lane is keyed by its owner (`track:N`, `bus:name`, `master`), its
//! parameter and its index. Each command changes one clip, keeps the project
//! valid, and is exactly one undo entry.
//!
//! Only a lane with a span is a clip. A lane that drives the whole timeline has
//! no edges to grab, so every gesture here refuses it and names the lane;
//! [`remove_automation_lane`] deletes either form.

use tauri::State;

use oc_dsl::ast;
use oc_dsl::timing::ProjectClock;

use crate::state::AppState;
use crate::state::{UndoEdit, UndoScope};

use super::fx::{lane_unit_value, owner_lanes, replace_owner_lanes, resolve_lane_index};
use super::shared::{ProjectSnapshot, resync_with_undo, snapshot};

/// Quarter notes a split keeps clear of either clip edge. A block position is
/// written as `bar:beat:subdivision` and one subdivision is a 16th note, so a
/// seam closer than that has no notation of its own.
const MIN_SPLIT_QUARTERS: f64 = 0.25;

/// Bars a duplicate searches past the requested position before it gives up.
const DUPLICATE_WINDOW_BARS: u32 = 64;

/// Sample rate used to evaluate the curve at a seam. An envelope lane's points
/// are sample-indexed, so the value at a quarter note does not depend on it.
const PROBE_SAMPLE_RATE: u32 = 48_000;

/// `bar:beat`, with the subdivision only when the position is off the beat.
fn bar_beat_text(position: &ast::BarBeat) -> String {
    if position.subdivision == 0 {
        format!("{}:{}", position.bar, position.beat)
    } else {
        format!(
            "{}:{}:{}",
            position.bar, position.beat, position.subdivision
        )
    }
}

/// The span as a reader sees it, or the whole timeline.
fn span_text(range: Option<ast::TimeRange>) -> String {
    match range {
        Some(range) => format!(
            "[{} - {}]",
            bar_beat_text(&range.start),
            bar_beat_text(&range.end)
        ),
        None => "the whole timeline".to_string(),
    }
}

/// The clip's span in quarter notes, or the refusal for a lane that has none.
///
/// A lane on its own tick grid (`[ppq=N]`) reads its positions in ticks rather
/// than on the project's notated grid, so a gesture that writes positions back
/// through the project clock would shift its points by the wrong amount.
fn clip_span(lane: &ast::AutomationLane, clock: &ProjectClock) -> Result<(f64, f64), String> {
    let Some(range) = lane.time_range else {
        return Err(format!(
            "Automation lane '{}' drives the whole timeline; a clip gesture needs a span",
            lane.param
        ));
    };
    if lane.ppq != 0 {
        return Err(format!(
            "Automation lane '{}' uses [ppq={}]; a clip gesture needs the project's notated grid",
            lane.param, lane.ppq
        ));
    }
    let start = clock.quarter_notes_at(&range.start)?;
    let end = clock.quarter_notes_at(&range.end)?;
    if end <= start {
        return Err(format!(
            "Automation lane '{}' has an empty span {}; fix the range first",
            lane.param,
            span_text(lane.time_range)
        ));
    }
    Ok((start, end))
}

/// A lane's span in quarter notes; a whole-timeline lane covers everything.
fn lane_span(lane: &ast::AutomationLane, clock: &ProjectClock) -> Result<(f64, f64), String> {
    match lane.time_range {
        Some(range) => Ok((
            clock.quarter_notes_at(&range.start)?,
            clock.quarter_notes_at(&range.end)?,
        )),
        None => Ok((f64::NEG_INFINITY, f64::INFINITY)),
    }
}

/// Refuse a candidate span that would take over time another lane on the same
/// target already writes. Both spans are named, so the message says what to
/// move out of the way.
fn check_free(
    lanes: &[ast::AutomationLane],
    skip: usize,
    param: &str,
    clock: &ProjectClock,
    start: f64,
    end: f64,
    candidate: &str,
) -> Result<(), String> {
    for (index, other) in lanes.iter().enumerate() {
        if index == skip || other.param != param {
            continue;
        }
        let (other_start, other_end) = lane_span(other, clock)?;
        if start < other_end && other_start < end {
            return Err(format!(
                "Automation clip {candidate} would overlap {} on '{param}'; \
                 one target is driven by one lane at a time",
                span_text(other.time_range)
            ));
        }
    }
    Ok(())
}

/// Shift the range and every point by one delta. The curve keeps its shape
/// because every relative distance is preserved.
pub(crate) fn apply_move_clip(
    project: &mut ast::Project,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    to: &ast::BarBeat,
) -> Result<(), String> {
    let clock = ProjectClock::new(&project.properties)?;
    let to_quarter = clock.quarter_notes_at(to)?;
    let mut lanes = owner_lanes(project, owner)?;
    let index = resolve_lane_index(&lanes, lane_param, lane_index)?;
    let lane = &lanes[index];
    let (start, end) = clip_span(lane, &clock)?;
    let duration = end - start;
    let new_end = to_quarter + duration;
    let range = ast::TimeRange {
        start: *to,
        end: clock.bar_beat_at_quarter(new_end)?,
    };
    check_free(
        &lanes,
        index,
        &lane.param,
        &clock,
        to_quarter,
        new_end,
        &span_text(Some(range)),
    )?;
    // Every point moves by the same delta, so the shape and the order hold.
    let delta = to_quarter - start;
    let mut points = Vec::with_capacity(lane.points.len());
    for point in &lane.points {
        let mut moved = point.clone();
        moved.position =
            clock.bar_beat_at_quarter(clock.quarter_notes_at(&point.position)? + delta)?;
        points.push(moved);
    }
    let lane = &mut lanes[index];
    lane.points = points;
    lane.time_range = Some(range);
    replace_owner_lanes(project, owner, lanes)?;
    Ok(())
}

/// Move one edge; the other edge and every point stay where they are.
///
/// A point left outside the new span keeps shaping the curve inside it, so a
/// shrunken clip still holds the opening or closing the points describe.
pub(crate) fn apply_resize_clip(
    project: &mut ast::Project,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    edge: &str,
    to: &ast::BarBeat,
) -> Result<(), String> {
    let clock = ProjectClock::new(&project.properties)?;
    let to_quarter = clock.quarter_notes_at(to)?;
    let mut lanes = owner_lanes(project, owner)?;
    let index = resolve_lane_index(&lanes, lane_param, lane_index)?;
    let lane = &lanes[index];
    let (start, end) = clip_span(lane, &clock)?;
    let range = lane.time_range.expect("clip_span proved a span");
    let (new_range, new_start, new_end) = match edge {
        "start" => {
            if to_quarter >= end {
                return Err(format!(
                    "Cannot move the start of {} to {}: it would cross the clip's end",
                    span_text(Some(range)),
                    bar_beat_text(to)
                ));
            }
            (
                ast::TimeRange {
                    start: *to,
                    end: range.end,
                },
                to_quarter,
                end,
            )
        }
        "end" => {
            if to_quarter <= start {
                return Err(format!(
                    "Cannot move the end of {} to {}: it would cross the clip's start",
                    span_text(Some(range)),
                    bar_beat_text(to)
                ));
            }
            (
                ast::TimeRange {
                    start: range.start,
                    end: *to,
                },
                start,
                to_quarter,
            )
        }
        other => {
            return Err(format!(
                "Resize edge must be 'start' or 'end', not '{other}'"
            ));
        }
    };
    check_free(
        &lanes,
        index,
        &lane.param,
        &clock,
        new_start,
        new_end,
        &span_text(Some(new_range)),
    )?;
    lanes[index].time_range = Some(new_range);
    replace_owner_lanes(project, owner, lanes)?;
    Ok(())
}

/// Copy a clip to a free position at or after the requested one.
///
/// The copy keeps the target, the span length, the mute flag, the curve
/// controls and every point value; only the positions move. When the requested
/// position is taken the search walks right past each blocker, so a duplicate
/// next to a neighbour lands after it. Returns the new lane's index.
pub(crate) fn apply_duplicate_clip(
    project: &mut ast::Project,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    at: &ast::BarBeat,
) -> Result<usize, String> {
    let clock = ProjectClock::new(&project.properties)?;
    let requested = clock.quarter_notes_at(at)?;
    let mut lanes = owner_lanes(project, owner)?;
    let index = resolve_lane_index(&lanes, lane_param, lane_index)?;
    let lane = &lanes[index];
    let (start, end) = clip_span(lane, &clock)?;
    let duration = end - start;
    let window = at.bar.saturating_add(DUPLICATE_WINDOW_BARS);
    let limit = clock.quarter_notes_at(&ast::BarBeat::new(window, 1, 0))?;
    let mut slot = requested;
    loop {
        // The window bounds the search, not a requested position that is free:
        // a clip longer than the window still copies where it was asked to.
        if slot > requested && slot + duration > limit {
            return Err(format!(
                "No free position for the copy of {} within {DUPLICATE_WINDOW_BARS} bars \
                 after {}: '{lane_param}' is driven there by another lane",
                span_text(lane.time_range),
                bar_beat_text(at)
            ));
        }
        let mut blocked: Option<f64> = None;
        // The source blocks its own position, so a copy asked for there walks on.
        for other in lanes.iter() {
            if other.param != lane.param {
                continue;
            }
            let (other_start, other_end) = lane_span(other, &clock)?;
            if slot < other_end && other_start < slot + duration {
                // A whole-timeline lane leaves no room anywhere on its target.
                if !other_end.is_finite() {
                    return Err(format!(
                        "No free position for the copy of {} on '{lane_param}': \
                         another lane drives that target over the whole timeline",
                        span_text(lane.time_range)
                    ));
                }
                blocked = Some(blocked.map_or(other_end, |current: f64| current.max(other_end)));
            }
        }
        match blocked {
            Some(next) => slot = next,
            None => break,
        }
    }
    let mut copy = lane.clone();
    copy.time_range = Some(ast::TimeRange {
        start: clock.bar_beat_at_quarter(slot)?,
        end: clock.bar_beat_at_quarter(slot + duration)?,
    });
    let delta = slot - start;
    for point in &mut copy.points {
        point.position =
            clock.bar_beat_at_quarter(clock.quarter_notes_at(&point.position)? + delta)?;
    }
    let new_index = lanes.len();
    lanes.push(copy);
    replace_owner_lanes(project, owner, lanes)?;
    Ok(new_index)
}

/// Split a clip at a position strictly inside its span.
///
/// The left lane keeps the head and gains a closing point at the seam; the right
/// lane starts at the seam with the same value, so the curve is continuous. A
/// point exactly on the seam stays on the right, and every other point stays on
/// its own side. The right lane is inserted directly after the left one.
pub(crate) fn apply_split_clip(
    project: &mut ast::Project,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    at: &ast::BarBeat,
) -> Result<usize, String> {
    let clock = ProjectClock::new(&project.properties)?;
    let split = clock.quarter_notes_at(at)?;
    let mut lanes = owner_lanes(project, owner)?;
    let index = resolve_lane_index(&lanes, lane_param, lane_index)?;
    let lane = &lanes[index];
    let (start, end) = clip_span(lane, &clock)?;
    if !(split > start && split < end) {
        return Err(format!(
            "Split position {} is outside the clip {}; a split must be strictly inside",
            bar_beat_text(at),
            span_text(lane.time_range)
        ));
    }
    if split - start < MIN_SPLIT_QUARTERS || end - split < MIN_SPLIT_QUARTERS {
        return Err(format!(
            "Split position {} is within one block grid step (a 16th note) of an edge of {}; \
             move it further in",
            bar_beat_text(at),
            span_text(lane.time_range)
        ));
    }
    if lane.is_lfo() {
        return Err(format!(
            "Lane '{}' is a waveform lane: its points describe one cycle, and each half would \
             restart that cycle. Switch the lane to envelope first",
            lane.param
        ));
    }
    // The seam value in the lane's own units, which is what a point carries.
    let mut probe = lane.clone();
    probe.muted = false;
    let seam_value =
        oc_dsl::automation::value_at(&probe, &clock, PROBE_SAMPLE_RATE, split, |value| {
            value
                .as_f64()
                .ok_or_else(|| "automation point value is not a number".to_string())
        })?
        .ok_or_else(|| {
            format!(
                "The clip {} writes no value at {}; there is nothing to split",
                span_text(lane.time_range),
                bar_beat_text(at)
            )
        })?;
    let seam = lane_unit_value(lane, seam_value);
    let mut left_points = Vec::with_capacity(lane.points.len() + 1);
    let mut right_points = Vec::with_capacity(lane.points.len() + 1);
    // The segment that ends at the seam keeps its curve on the right lane's
    // first point, so both halves draw the shape the whole clip drew.
    let mut seam_curve = ast::Curve::Linear;
    let mut seam_tension = None;
    for point in &lane.points {
        if clock.quarter_notes_at(&point.position)? < split {
            seam_curve = point.curve;
            seam_tension = point.tension;
            left_points.push(point.clone());
        } else {
            right_points.push(point.clone());
        }
    }
    left_points.push(ast::AutomationPoint {
        position: *at,
        value: seam.clone(),
        curve: ast::Curve::Linear,
        tension: None,
    });
    let opens_on_the_seam = match right_points.first() {
        Some(point) => clock.quarter_notes_at(&point.position)? <= split,
        None => false,
    };
    if !opens_on_the_seam {
        right_points.insert(
            0,
            ast::AutomationPoint {
                position: *at,
                value: seam,
                curve: seam_curve,
                tension: seam_tension,
            },
        );
    }
    let range = lane.time_range.expect("clip_span proved a span");
    let mut left = lane.clone();
    left.time_range = Some(ast::TimeRange {
        start: range.start,
        end: *at,
    });
    left.points = left_points;
    let mut right = lane.clone();
    right.time_range = Some(ast::TimeRange {
        start: *at,
        end: range.end,
    });
    right.points = right_points;
    lanes.insert(index + 1, right);
    lanes[index] = left;
    replace_owner_lanes(project, owner, lanes)?;
    Ok(index + 1)
}

/// Remove a clip lane. A whole-timeline lane is not a clip, so it is refused
/// instead of silently deleting a lane the gesture never addressed.
pub(crate) fn apply_delete_clip(
    project: &mut ast::Project,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
) -> Result<(), String> {
    let mut lanes = owner_lanes(project, owner)?;
    let index = resolve_lane_index(&lanes, lane_param, lane_index)?;
    if lanes[index].time_range.is_none() {
        return Err(format!(
            "Automation lane '{}' drives the whole timeline; this command deletes clips only",
            lanes[index].param
        ));
    }
    lanes.remove(index);
    replace_owner_lanes(project, owner, lanes)?;
    Ok(())
}

/// Remove one lane of any form.
///
/// A whole-timeline lane has no clip to address, so the clip gestures refuse it
/// and this command is the only way to delete it. The lane leaves the next
/// graph with the project resync, so a sounding lane stops writing its effect.
pub(crate) fn apply_remove_lane(
    project: &mut ast::Project,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
) -> Result<(), String> {
    let mut lanes = owner_lanes(project, owner)?;
    let index = resolve_lane_index(&lanes, lane_param, lane_index)?;
    lanes.remove(index);
    replace_owner_lanes(project, owner, lanes)?;
    Ok(())
}

/// Publish a candidate project: the document must still validate before it
/// replaces the live one, and the edit is one undo entry.
fn commit(
    inner: &mut crate::state::AppStateInner,
    candidate: ast::Project,
    label: &str,
) -> Result<(), String> {
    oc_dsl::validate(&candidate).map_err(|errors| {
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    let old = std::mem::replace(&mut inner.project, candidate);
    resync_with_undo(inner, old, UndoEdit::new(label, UndoScope::Automation))?;
    Ok(())
}

pub(crate) fn move_automation_clip_inner(
    inner: &mut crate::state::AppStateInner,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    to: ast::BarBeat,
) -> Result<(), String> {
    let mut candidate = inner.project.clone();
    apply_move_clip(&mut candidate, owner, lane_param, lane_index, &to)?;
    commit(inner, candidate, "move automation clip")
}

pub(crate) fn resize_automation_clip_inner(
    inner: &mut crate::state::AppStateInner,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    edge: &str,
    to: ast::BarBeat,
) -> Result<(), String> {
    let mut candidate = inner.project.clone();
    apply_resize_clip(&mut candidate, owner, lane_param, lane_index, edge, &to)?;
    commit(inner, candidate, "resize automation clip")
}

pub(crate) fn duplicate_automation_clip_inner(
    inner: &mut crate::state::AppStateInner,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    at: ast::BarBeat,
) -> Result<usize, String> {
    let mut candidate = inner.project.clone();
    let index = apply_duplicate_clip(&mut candidate, owner, lane_param, lane_index, &at)?;
    commit(inner, candidate, "duplicate automation clip")?;
    Ok(index)
}

pub(crate) fn split_automation_clip_inner(
    inner: &mut crate::state::AppStateInner,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    at: ast::BarBeat,
) -> Result<usize, String> {
    let mut candidate = inner.project.clone();
    let index = apply_split_clip(&mut candidate, owner, lane_param, lane_index, &at)?;
    commit(inner, candidate, "split automation clip")?;
    Ok(index)
}

pub(crate) fn delete_automation_clip_inner(
    inner: &mut crate::state::AppStateInner,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
) -> Result<(), String> {
    let mut candidate = inner.project.clone();
    apply_delete_clip(&mut candidate, owner, lane_param, lane_index)?;
    commit(inner, candidate, "delete automation clip")
}

pub(crate) fn remove_automation_lane_inner(
    inner: &mut crate::state::AppStateInner,
    owner: &str,
    lane_param: &str,
    lane_index: Option<usize>,
) -> Result<(), String> {
    let mut candidate = inner.project.clone();
    apply_remove_lane(&mut candidate, owner, lane_param, lane_index)?;
    commit(inner, candidate, "remove automation lane")
}

/// Shift one automation clip; every point moves by the same delta.
#[tauri::command]
pub fn move_automation_clip(
    owner: String,
    lane_param: String,
    lane_index: Option<usize>,
    to_bar: u32,
    to_beat: u32,
    to_subdivision: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    move_automation_clip_inner(
        &mut inner,
        &owner,
        &lane_param,
        lane_index,
        ast::BarBeat::new(to_bar, to_beat, to_subdivision),
    )?;
    Ok(snapshot(&inner))
}

/// Move one edge of an automation clip.
#[expect(
    clippy::too_many_arguments,
    reason = "Tauri preserves the flat IPC argument keys consumed by existing clients"
)]
/// Resize an automation clip.
#[tauri::command]
pub fn resize_automation_clip(
    owner: String,
    lane_param: String,
    lane_index: Option<usize>,
    edge: String,
    to_bar: u32,
    to_beat: u32,
    to_subdivision: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    resize_automation_clip_inner(
        &mut inner,
        &owner,
        &lane_param,
        lane_index,
        &edge,
        ast::BarBeat::new(to_bar, to_beat, to_subdivision),
    )?;
    Ok(snapshot(&inner))
}

/// Copy an automation clip to the first free position at or after `at`.
#[tauri::command]
pub fn duplicate_automation_clip(
    owner: String,
    lane_param: String,
    lane_index: Option<usize>,
    at_bar: u32,
    at_beat: u32,
    at_subdivision: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    duplicate_automation_clip_inner(
        &mut inner,
        &owner,
        &lane_param,
        lane_index,
        ast::BarBeat::new(at_bar, at_beat, at_subdivision),
    )?;
    Ok(snapshot(&inner))
}

/// Split an automation clip at a position strictly inside its span.
#[tauri::command]
pub fn split_automation_clip(
    owner: String,
    lane_param: String,
    lane_index: Option<usize>,
    at_bar: u32,
    at_beat: u32,
    at_subdivision: u32,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    split_automation_clip_inner(
        &mut inner,
        &owner,
        &lane_param,
        lane_index,
        ast::BarBeat::new(at_bar, at_beat, at_subdivision),
    )?;
    Ok(snapshot(&inner))
}

/// Remove an automation clip lane.
#[tauri::command]
pub fn delete_automation_clip(
    owner: String,
    lane_param: String,
    lane_index: Option<usize>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    delete_automation_clip_inner(&mut inner, &owner, &lane_param, lane_index)?;
    Ok(snapshot(&inner))
}

/// Remove one automation lane, clip or whole-timeline.
#[tauri::command]
pub fn remove_automation_lane(
    owner: String,
    lane_param: String,
    lane_index: Option<usize>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    remove_automation_lane_inner(&mut inner, &owner, &lane_param, lane_index)?;
    Ok(snapshot(&inner))
}
