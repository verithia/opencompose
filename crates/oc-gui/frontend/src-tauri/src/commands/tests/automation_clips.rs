//! Timeline gestures on automation clips: move, resize, duplicate, split, delete.

use super::support::test_state;
use crate::audio::PlaybackState;
use crate::commands::automation::{AutomationReadout, automation_value_at_inner};
use crate::commands::automation_clips::{
    delete_automation_clip_inner, duplicate_automation_clip_inner, move_automation_clip_inner,
    remove_automation_lane_inner, resize_automation_clip_inner, split_automation_clip_inner,
};
use crate::state::AppStateInner;
use oc_dsl::ast;

const LANE: &str = "gain.factor";

/// Two clips on one target: `[1:1 - 3:1]` and `[5:1 - 7:1]`.
const TWO_CLIPS: &str = "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1:0 - 3:1:0] {1:1:0=1, 1:3:0=0.5}\n  automation: gain.factor [5:1:0 - 7:1:0] {5:1:0=0.2, 5:3:0=0.8}\n";

/// One clip whose curve reaches 0.25 at beat 2 of bar 2, so a seam in between
/// carries a value that is neither point's.
const ONE_CLIP: &str = "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1:0 - 3:1:0] {1:1:0=1, 2:2:0=0.25}\n";

/// A lane with no span; every clip gesture must refuse it.
const WHOLE: &str = "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=1, 1:3:0=0.5}\n";

fn state(source: &str) -> AppStateInner {
    let mut inner = test_state();
    inner.project = oc_dsl::parse(source).expect("test project parses");
    inner.audio.sample_rate = 48_000;
    inner
}

fn lanes(inner: &AppStateInner) -> Vec<&ast::AutomationLane> {
    inner.project.tracks[0].automation().collect()
}

fn span(lane: &ast::AutomationLane) -> (ast::BarBeat, ast::BarBeat) {
    let range = lane.time_range.expect("the lane is a clip");
    (range.start, range.end)
}

/// A point value in the lane's own units.
fn value(point: &ast::AutomationPoint) -> f64 {
    point.value.as_f64().expect("a numeric point")
}

/// What the lane writes at a quarter-note position, in the lane's units.
fn written(inner: &AppStateInner, index: usize, quarter: f64) -> Option<f64> {
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
    oc_dsl::automation::value_at(lanes(inner)[index], &clock, 48_000, quarter, |value| {
        Ok(value.as_f64().expect("a numeric point"))
    })
    .expect("the lane evaluates")
}

#[test]
fn moving_an_automation_clip_shifts_its_range_and_every_point_and_is_one_undo() {
    let mut inner = state(TWO_CLIPS);
    move_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(2, 3, 0),
    )
    .unwrap();
    assert_eq!(inner.undo.undo_entries().len(), 1, "a move is one entry");

    // Bar 2 beat 3 is quarter 6 and the clip is two bars long, so it ends at
    // quarter 14: bar 4 beat 3.
    let (start, end) = span(lanes(&inner)[0]);
    assert_eq!(start, ast::BarBeat::new(2, 3, 0));
    assert_eq!(end, ast::BarBeat::new(4, 3, 0));
    let points = &lanes(&inner)[0].points;
    assert_eq!(points[0].position, ast::BarBeat::new(2, 3, 0));
    assert_eq!(points[1].position, ast::BarBeat::new(3, 1, 0));
    assert_eq!(value(&points[0]), 1.0, "values do not move");
    assert_eq!(value(&points[1]), 0.5);
    // The other lane keeps its own span and points.
    assert_eq!(span(lanes(&inner)[1]).0, ast::BarBeat::new(5, 1, 0));

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(span(lanes(&inner)[0]).0, ast::BarBeat::new(1, 1, 0));
    assert_eq!(
        lanes(&inner)[0].points[1].position,
        ast::BarBeat::new(1, 3, 0)
    );
}

#[test]
fn a_move_that_would_overlap_another_lane_is_refused_and_changes_nothing() {
    let mut inner = state(TWO_CLIPS);
    let before = oc_dsl::serialize(&inner.project);

    // Quarter 12 starts bar 4: `[4:1 - 6:1]` runs into `[5:1 - 7:1]`.
    let error = move_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(4, 1, 0),
    )
    .unwrap_err();
    assert!(
        error.contains("[4:1 - 6:1]"),
        "the move names its span: {error}"
    );
    assert!(
        error.contains("[5:1 - 7:1]"),
        "and the lane it hits: {error}"
    );
    assert!(
        error.contains("one target is driven by one lane"),
        "{error}"
    );
    assert_eq!(oc_dsl::serialize(&inner.project), before, "nothing moved");
    assert_eq!(
        inner.undo.undo_entries().len(),
        0,
        "a refusal adds no history"
    );

    // Adjacent edges are allowed: `[3:1 - 5:1]` touches `[5:1 - 7:1]`.
    move_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(3, 1, 0),
    )
    .unwrap();
    assert_eq!(span(lanes(&inner)[0]).1, ast::BarBeat::new(5, 1, 0));
}

#[test]
fn a_resize_stops_at_the_other_edge_and_keeps_the_points_outside() {
    let mut inner = state(TWO_CLIPS);

    // The end cannot pass the start, and the refusal leaves the span alone.
    let error = resize_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        "end",
        ast::BarBeat::new(1, 1, 0),
    )
    .unwrap_err();
    assert!(error.contains("would cross the clip's start"), "{error}");
    assert_eq!(
        span(lanes(&inner)[0]),
        (ast::BarBeat::new(1, 1, 0), ast::BarBeat::new(3, 1, 0))
    );
    assert_eq!(inner.undo.undo_entries().len(), 0);

    // Moving the head in keeps the point that now sits before the span.
    resize_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        "start",
        ast::BarBeat::new(2, 1, 0),
    )
    .unwrap();
    assert_eq!(inner.undo.undo_entries().len(), 1);
    assert_eq!(
        span(lanes(&inner)[0]),
        (ast::BarBeat::new(2, 1, 0), ast::BarBeat::new(3, 1, 0))
    );
    assert_eq!(lanes(&inner)[0].points.len(), 2, "no point is dropped");

    // Moving the tail out is accepted; the range ends where it was asked.
    resize_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        "end",
        ast::BarBeat::new(2, 3, 0),
    )
    .unwrap();
    assert_eq!(span(lanes(&inner)[0]).1, ast::BarBeat::new(2, 3, 0));

    // A resize that would run into the next lane is refused with both spans.
    let error = resize_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        "end",
        ast::BarBeat::new(5, 2, 0),
    )
    .unwrap_err();
    assert!(error.contains("[2:1 - 5:2]"), "{error}");
    assert!(error.contains("[5:1 - 7:1]"), "{error}");
    assert_eq!(
        span(lanes(&inner)[0]).1,
        ast::BarBeat::new(2, 3, 0),
        "unchanged"
    );
}

#[test]
fn duplicate_picks_a_free_slot_and_keeps_point_values() {
    let mut inner = state(TWO_CLIPS);

    // The requested bar is occupied by the clip itself, so the copy lands on
    // the first free slot: `[3:1 - 5:1]`.
    let index = duplicate_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(1, 1, 0),
    )
    .unwrap();
    assert_eq!(index, 2, "the copy is appended to the owner's lanes");
    assert_eq!(
        inner.undo.undo_entries().len(),
        1,
        "a duplicate is one entry"
    );
    let copy = &lanes(&inner)[2];
    assert_eq!(copy.param, LANE);
    assert_eq!(
        span(copy),
        (ast::BarBeat::new(3, 1, 0), ast::BarBeat::new(5, 1, 0))
    );
    assert_eq!(value(&copy.points[0]), 1.0);
    assert_eq!(value(&copy.points[1]), 0.5);
    assert_eq!(copy.points[0].position, ast::BarBeat::new(3, 1, 0));
    assert_eq!(copy.points[1].position, ast::BarBeat::new(3, 3, 0));
    // The source is untouched.
    assert_eq!(span(lanes(&inner)[0]).0, ast::BarBeat::new(1, 1, 0));

    // A free requested position is used as asked.
    let index = duplicate_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(9, 1, 0),
    )
    .unwrap();
    assert_eq!(
        span(lanes(&inner)[index]),
        (ast::BarBeat::new(9, 1, 0), ast::BarBeat::new(11, 1, 0))
    );
}

#[test]
fn duplicate_refuses_when_the_window_has_no_room() {
    // A whole-timeline lane leaves no free position on its target.
    let mut inner = state(
        "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1:0 - 2:1:0] {1:1:0=1}\n  automation: gain.factor {1:1:0=0.5}\n",
    );
    let error = duplicate_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(3, 1, 0),
    )
    .unwrap_err();
    assert!(error.contains("No free position"), "{error}");
    assert!(error.contains("whole timeline"), "{error}");
    assert_eq!(lanes(&inner).len(), 2, "no lane was added");
    assert_eq!(inner.undo.undo_entries().len(), 0);
}

#[test]
fn duplicate_gives_up_when_the_window_has_no_room() {
    // The next lane covers everything past the clip, so the free slot the
    // search walks to sits beyond the 64-bar window it states.
    let mut inner = state(
        "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1:0 - 2:1:0] {1:1:0=1}\n  automation: gain.factor [2:1:0 - 66:1:0] {2:1:0=0.5}\n",
    );
    let error = duplicate_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(1, 1, 0),
    )
    .unwrap_err();
    assert!(error.contains("within 64 bars after 1:1"), "{error}");
    assert_eq!(lanes(&inner).len(), 2, "no lane was added");
    assert_eq!(inner.undo.undo_entries().len(), 0);
}

#[test]
fn splitting_a_clip_keeps_the_value_at_the_seam() {
    let mut inner = state(ONE_CLIP);
    let original_points = lanes(&inner)[0].points.clone();
    let before = written(&inner, 0, 4.0).expect("the whole clip writes at bar 2");
    assert!(
        (before - 0.4).abs() < 1e-12,
        "the pre-split curve is 0.4: {before}"
    );

    let index = split_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(2, 1, 0),
    )
    .unwrap();
    assert_eq!(index, 1, "the right lane follows the left one");
    assert_eq!(inner.undo.undo_entries().len(), 1, "a split is one entry");
    assert_eq!(lanes(&inner).len(), 2);

    let left = &lanes(&inner)[0];
    let right = &lanes(&inner)[1];
    assert_eq!(
        span(left),
        (ast::BarBeat::new(1, 1, 0), ast::BarBeat::new(2, 1, 0))
    );
    assert_eq!(
        span(right),
        (ast::BarBeat::new(2, 1, 0), ast::BarBeat::new(3, 1, 0))
    );
    // The two independent copies retain the exact original curve anchors.
    assert_eq!(left.points, original_points);
    assert_eq!(right.points, original_points);

    // Numerically: the right lane writes the seam value there, and the left
    // lane approaches it from below.
    let at_seam = written(&inner, 1, 4.0).expect("the right lane writes at the seam");
    assert!((at_seam - before).abs() < 1e-12, "{at_seam}");
    let before_seam = written(&inner, 0, 4.0 - 1.0 / 48_000.0).expect("the left lane writes");
    assert!((before_seam - before).abs() < 1e-4, "{before_seam}");

    // The document still passes the overlap rule.
    assert!(oc_dsl::validate(&inner.project).is_ok());

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(lanes(&inner).len(), 1);
    assert_eq!(
        lanes(&inner)[0].points.len(),
        2,
        "the original anchors remain"
    );
}

#[test]
fn a_split_outside_the_clip_or_on_its_edge_is_refused() {
    let mut inner = state(ONE_CLIP);

    for at in [ast::BarBeat::new(4, 1, 0), ast::BarBeat::new(1, 1, 0)] {
        let error =
            split_automation_clip_inner(&mut inner, "track:0", LANE, Some(0), at).unwrap_err();
        assert!(error.contains("must be strictly inside"), "{error}");
    }
    assert_eq!(lanes(&inner).len(), 1, "nothing was split");
    assert_eq!(inner.undo.undo_entries().len(), 0);

    // Exactly one block step inside the head is the closest seam that fits, so
    // it is the last accepted position.
    split_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(1, 1, 1),
    )
    .unwrap();
    assert_eq!(lanes(&inner).len(), 2);
    assert_eq!(span(lanes(&inner)[0]).1, ast::BarBeat::new(1, 1, 1));
}

#[test]
fn a_split_closer_than_one_grid_step_is_refused() {
    // A 6/8 bar notates a 32nd, so a seam can sit inside one block step.
    let mut inner = state(
        "@project \"auto\"\n  tempo: 120\n  time_signature: 6/8\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1:0 - 3:1:0] {1:1:0=1, 2:1:0=0.25}\n",
    );
    let error = split_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(1, 1, 1),
    )
    .unwrap_err();
    assert!(error.contains("one block grid step"), "{error}");
    assert_eq!(lanes(&inner).len(), 1, "nothing was split");
    assert_eq!(inner.undo.undo_entries().len(), 0);
}

#[test]
fn deleting_removes_only_a_ranged_lane() {
    let mut inner = state(TWO_CLIPS);
    delete_automation_clip_inner(&mut inner, "track:0", LANE, Some(0)).unwrap();
    assert_eq!(inner.undo.undo_entries().len(), 1, "a delete is one entry");
    assert_eq!(lanes(&inner).len(), 1);
    assert_eq!(span(lanes(&inner)[0]).0, ast::BarBeat::new(5, 1, 0));

    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(lanes(&inner).len(), 2);

    // A whole-timeline lane is not a clip; the delete is refused in words.
    let mut whole = state(WHOLE);
    let error = delete_automation_clip_inner(&mut whole, "track:0", LANE, Some(0)).unwrap_err();
    assert!(error.contains("whole timeline"), "{error}");
    assert_eq!(lanes(&whole).len(), 1);
    assert_eq!(whole.undo.undo_entries().len(), 0);
}

#[test]
fn every_clip_gesture_refuses_a_whole_timeline_lane() {
    let mut inner = state(WHOLE);
    let error = move_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        None,
        ast::BarBeat::new(2, 1, 0),
    )
    .unwrap_err();
    assert!(error.contains("a clip gesture needs a span"), "{error}");
    let error = resize_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        None,
        "end",
        ast::BarBeat::new(2, 1, 0),
    )
    .unwrap_err();
    assert!(error.contains("a clip gesture needs a span"), "{error}");
    let error = duplicate_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        None,
        ast::BarBeat::new(2, 1, 0),
    )
    .unwrap_err();
    assert!(error.contains("a clip gesture needs a span"), "{error}");
    let error = split_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        None,
        ast::BarBeat::new(2, 1, 0),
    )
    .unwrap_err();
    assert!(error.contains("a clip gesture needs a span"), "{error}");
    assert_eq!(inner.undo.undo_entries().len(), 0);
}

#[test]
fn a_tick_grid_lane_refuses_clip_gestures() {
    // `[ppq=N]` reads the lane's positions in ticks, not on the project grid.
    let mut inner = state(
        "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1:0 - 3:1:0] [ppq=960] {1:1:0=1}\n",
    );
    let error = move_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(2, 1, 0),
    )
    .unwrap_err();
    assert!(error.contains("[ppq=960]"), "{error}");
    assert_eq!(inner.undo.undo_entries().len(), 0);
}

#[test]
fn a_gesture_on_a_bus_lane_uses_the_same_owner_key() {
    let mut inner = state(
        "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@bus b\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1:0 - 3:1:0] {1:1:0=1}\n",
    );
    move_automation_clip_inner(
        &mut inner,
        "bus:b",
        LANE,
        Some(0),
        ast::BarBeat::new(3, 1, 0),
    )
    .unwrap();
    assert_eq!(
        span(&inner.project.buses[0].automation[0]).0,
        ast::BarBeat::new(3, 1, 0)
    );
    // A wrong owner is refused instead of editing another owner's lane.
    let error = move_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(1, 1, 0),
    )
    .unwrap_err();
    assert!(error.contains("out of range"), "{error}");
}

#[test]
fn removing_a_playing_whole_timeline_lane_is_one_undo_and_stops_driving() {
    let mut inner = state(WHOLE);
    inner.audio.playback_state = PlaybackState::Playing;
    let driving =
        |inner: &AppStateInner| automation_value_at_inner(inner, "track:0", LANE, Some(0), 0.0);
    assert!(
        matches!(driving(&inner), Ok(AutomationReadout::Driven { .. })),
        "the lane drives the gain before the removal"
    );

    remove_automation_lane_inner(&mut inner, "track:0", LANE, Some(0)).unwrap();
    assert!(lanes(&inner).is_empty(), "the lane is gone");
    assert_eq!(inner.undo.undo_entries().len(), 1, "a removal is one entry");
    let answer = driving(&inner).unwrap();
    assert!(
        matches!(answer, AutomationReadout::NotDriven { .. }),
        "the removed lane still drives the gain: {answer:?}"
    );

    // One undo restores the lane and its value.
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(lanes(&inner).len(), 1, "undo did not restore the lane");
    assert!(
        matches!(driving(&inner), Ok(AutomationReadout::Driven { .. })),
        "the restored lane does not drive the gain"
    );
}

#[test]
fn removing_a_clip_lane_also_leaves_the_other_lane_alone() {
    let mut inner = state(TWO_CLIPS);
    remove_automation_lane_inner(&mut inner, "track:0", LANE, Some(0)).unwrap();
    assert_eq!(lanes(&inner).len(), 1, "one lane was removed");
    assert_eq!(
        span(lanes(&inner)[0]).0,
        ast::BarBeat::new(5, 1, 0),
        "the wrong lane was removed"
    );
    assert_eq!(inner.undo.undo_entries().len(), 1);
}

#[test]
fn split_compact_exponential_keeps_original_anchors_and_applies_mapping_only_once() {
    let source = "@project\n  tempo: 120\n@track t | instrument: synth/keys\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1 - 3:1] mapping(exponential, curvature=2, scale=-1, offset=2) {1:1=0.2 curve=exponential curvature=-6, 3:1=0.8}\n";
    let mut project = oc_dsl::parse(source).unwrap();
    let original = project.tracks[0].automation().next().unwrap().clone();
    crate::commands::automation_clips::apply_split_clip(
        &mut project,
        "track:0",
        LANE,
        Some(0),
        &ast::BarBeat::new(2, 1, 0),
    )
    .unwrap();
    let lanes: Vec<_> = project.tracks[0].automation().collect();
    assert_eq!(lanes.len(), 2);
    for lane in &lanes {
        assert_eq!(lane.points, original.points);
        assert_eq!(lane.value_mapping, original.value_mapping);
    }
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    for rate in [8_000, 44_100, 48_000, 96_000] {
        let prepare = |lane: &ast::AutomationLane| {
            oc_dsl::automation::LaneCurve::new(lane, &clock, rate, |value| {
                value.as_f64().ok_or("numeric".into())
            })
            .unwrap()
        };
        let before = prepare(&original);
        let left = prepare(lanes[0]);
        let right = prepare(lanes[1]);
        for frame in (0..rate * 4).step_by(37) {
            let expected = before.envelope_value(frame as f64).unwrap();
            let actual = left
                .envelope_value(frame as f64)
                .or_else(|| right.envelope_value(frame as f64))
                .unwrap();
            assert!(
                (actual - expected).abs() < 2e-14,
                "rate{rate}frame{frame}: {actual}!={expected}"
            );
        }
    }
}

#[test]
fn split_on_discontinuity_keeps_the_left_limit_and_right_write() {
    let mut project=oc_dsl::parse("@project\n  tempo: 120\n@track t | instrument: synth/keys\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1 - 2:1] discontinuities mapping(linear, scale=-1, offset=1) {1:1=0, 1:3=1, 1:3=0, 2:1=0}\n").unwrap();
    let original = project.tracks[0].automation().next().unwrap().clone();
    crate::commands::automation_clips::apply_split_clip(
        &mut project,
        "track:0",
        LANE,
        Some(0),
        &ast::BarBeat::new(1, 3, 0),
    )
    .unwrap();
    let lanes: Vec<_> = project.tracks[0].automation().collect();
    assert_eq!(lanes[0].points, original.points);
    assert_eq!(lanes[1].points, original.points);
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let prepare = |lane: &ast::AutomationLane| {
        oc_dsl::automation::LaneCurve::new(lane, &clock, 48_000, |value| {
            value.as_f64().ok_or("numeric".into())
        })
        .unwrap()
    };
    let before = prepare(&original);
    let left = prepare(lanes[0]);
    let right = prepare(lanes[1]);
    for frame in [0.0, 24_000.0, 47_999.0, 48_000.0, 48_001.0, 95_999.0] {
        assert_eq!(
            before.envelope_value(frame),
            left.envelope_value(frame)
                .or_else(|| right.envelope_value(frame))
        );
    }
}

#[test]
fn agent_subtree_edit_rejects_mapped_output_before_publishing_or_undo() {
    use crate::commands::ai::{apply_edit_to_project, validate_edit_in_project};
    let mut inner = state(WHOLE);
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    let original_project = inner.project.clone();
    let original_dsl = inner.dsl_source.clone();
    let track = oc_dsl::ops::extract(&inner.project, "track:t")
        .unwrap()
        .dsl_text;
    let maximum = oc_device_schema::find_device("gain")
        .unwrap()
        .param("factor")
        .unwrap()
        .max;
    let edit = track.replace(
        "automation: gain.factor",
        &format!(
            "automation: gain.factor mapping(linear, scale={})",
            maximum + 1.0
        ),
    );
    let preview = validate_edit_in_project(&inner, edit.clone(), "track:t".into());
    assert!(!preview.valid);
    assert!(
        preview.errors.iter().any(|e| e.contains("recipient range")),
        "{:?}",
        preview.errors
    );
    let error = apply_edit_to_project(&mut inner, edit, "track:t".into(), original_dsl.clone())
        .unwrap_err();
    assert!(error.contains("recipient range"), "{error}");
    assert_eq!(inner.project, original_project);
    assert_eq!(inner.dsl_source, original_dsl);
    assert_eq!(inner.undo.undo_entries().len(), 0);
}

#[test]
fn split_preserves_all_curve_kinds_across_tempo_sample_rates_and_units() {
    for curve in [
        "linear",
        "stairs",
        "half_sine",
        "smooth",
        "exponential curvature=-4",
    ] {
        for units in [false, true] {
            let (effect, parameter, points) = if units {
                (
                    "limiter: ceiling=-1dB",
                    "limiter.ceiling",
                    format!("1:1=-6dB curve={curve}, 2:1=-1dB"),
                )
            } else {
                (
                    "gain: factor=1",
                    "gain.factor",
                    format!("1:1=0.2 curve={curve}, 2:1=0.8"),
                )
            };
            let source = format!(
                "@project\n  tempo: 120\n  tempo_map:\n    1:2 = 60 bpm\n@track t | instrument: synth/keys\n  effects:\n    - {effect}\n  automation: {parameter} [1:1 - 2:1] {{{points}}}\n"
            );
            let mut project = oc_dsl::parse(&source).unwrap();
            let original = project.tracks[0].automation().next().unwrap().clone();
            crate::commands::automation_clips::apply_split_clip(
                &mut project,
                "track:0",
                parameter,
                Some(0),
                &ast::BarBeat::new(1, 2, 0),
            )
            .unwrap();
            let lanes: Vec<_> = project.tracks[0].automation().collect();
            assert!(lanes.iter().all(|lane| lane.points == original.points));
            let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
            for rate in [8_000, 44_100, 48_000, 96_000] {
                let prepare = |lane: &ast::AutomationLane| {
                    oc_dsl::automation::LaneCurve::new(lane, &clock, rate, |value| {
                        value.as_f64().ok_or("numeric".into())
                    })
                    .unwrap()
                };
                let before = prepare(&original);
                let left = prepare(lanes[0]);
                let right = prepare(lanes[1]);
                let end = clock.sample_at_quarter(4.0, rate).unwrap();
                let seam = clock.sample_at_quarter(1.0, rate).unwrap();
                for frame in (0..=end)
                    .step_by(113)
                    .chain([seam - 1, seam, seam + 1, end - 1, end])
                {
                    assert_eq!(
                        before.envelope_value(frame as f64),
                        left.envelope_value(frame as f64)
                            .or_else(|| right.envelope_value(frame as f64)),
                        "{curve} units{units} rate{rate} frame{frame}"
                    );
                }
            }
        }
    }
}

#[test]
fn moving_a_split_copy_before_its_original_anchor_fails_atomically() {
    let mut inner = state(ONE_CLIP);
    split_automation_clip_inner(
        &mut inner,
        "track:0",
        LANE,
        Some(0),
        ast::BarBeat::new(2, 1, 0),
    )
    .unwrap();
    remove_automation_lane_inner(&mut inner, "track:0", LANE, Some(0)).unwrap();
    let before = inner.project.clone();
    let undo = inner.undo.undo_entries().len();
    assert!(
        move_automation_clip_inner(
            &mut inner,
            "track:0",
            LANE,
            Some(0),
            ast::BarBeat::new(1, 1, 0)
        )
        .is_err()
    );
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.undo_entries().len(), undo);
}
