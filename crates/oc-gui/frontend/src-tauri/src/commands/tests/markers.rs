//! Timeline markers and the session loop region: the command surface, the undo
//! entries a gesture produces, the refusals, and the boundary between playback
//! state and the offline renderer.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;

use oc_dsl::ast;

use super::support::test_state;
use crate::commands::markers::{
    apply_add_marker, apply_move_marker, apply_remove_marker, apply_rename_marker,
};
use crate::commands::sections::parse_position;
use crate::commands::shared::resync_with_undo;
use crate::state::{UndoEdit, UndoScope};

const REPEATED: &str = r#"@ocp "0.2.0"

@project "repeat"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.open_synth"

@track pad | instrument: synth/pad, preset: tone
  @phrase held [1:1 - 5:1]
    C4 w

@section verse [1:1 - 5:1]
  repeat: 3
"#;

const SONG: &str = r#"@ocp "0.2.0"

@project "markers"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.open_synth"

@track pad | instrument: synth/pad, preset: tone
  @phrase held [1:1 - 5:1]
    C4 w
"#;

fn project() -> ast::Project {
    let project = oc_dsl::parse(SONG).expect("fixture parses");
    oc_dsl::validate(&project).expect("fixture validates");
    project
}

fn marker_names(project: &ast::Project) -> Vec<&str> {
    project
        .markers
        .iter()
        .map(|marker| marker.name.as_str())
        .collect()
}

fn add(project: &mut ast::Project, name: &str, at: &str) {
    apply_add_marker(project, name, parse_position(at).unwrap()).unwrap();
}

#[test]
fn markers_add_in_written_order_and_survive_save_load() {
    let mut project = project();
    add(&mut project, "chorus", "9:1");
    add(&mut project, "verse", "1:1");
    add(&mut project, "bridge", "13:1:2");

    assert_eq!(marker_names(&project), ["verse", "chorus", "bridge"]);
    let text = oc_dsl::serialize(&project);
    assert!(
        text.contains("@marker verse at 1:1")
            && text.contains("@marker chorus at 9:1")
            && text.contains("@marker bridge at 13:1:2"),
        "{text}"
    );
    assert_eq!(oc_dsl::parse(&text).unwrap(), project);
}

#[test]
fn a_marker_past_the_project_end_is_kept() {
    let mut project = project();
    add(&mut project, "outro", "99:1");
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    assert_eq!(reopened.markers[0].position, ast::BarBeat::new(99, 1, 0));
}

#[test]
fn marker_add_move_rename_and_delete_are_undoable() {
    let mut inner = test_state();
    inner.project = project();

    let old_project = inner.project.clone();
    apply_add_marker(&mut inner.project, "verse", parse_position("1:1").unwrap()).unwrap();
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("add marker", UndoScope::Project),
    )
    .expect("resync");
    assert_eq!(inner.undo.undo.len(), 1);
    assert!(inner.dsl_source.contains("@marker verse at 1:1"));
    inner.undo.undo(&mut inner.project).unwrap();
    assert!(inner.project.markers.is_empty(), "undo removes the marker");
    inner.undo.redo(&mut inner.project).unwrap();
    assert_eq!(marker_names(&inner.project), ["verse"]);

    // Move: one entry, and undo restores the old position.
    let old_project = inner.project.clone();
    assert!(
        apply_move_marker(&mut inner.project, "verse", parse_position("9:1").unwrap()).unwrap()
    );
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("move marker", UndoScope::Project),
    )
    .expect("resync");
    assert_eq!(
        inner.project.markers[0].position,
        ast::BarBeat::new(9, 1, 0)
    );
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(
        inner.project.markers[0].position,
        ast::BarBeat::new(1, 1, 0)
    );

    // Rename.
    let old_project = inner.project.clone();
    assert!(apply_rename_marker(&mut inner.project, "verse", "intro").unwrap());
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("rename marker", UndoScope::Project),
    )
    .expect("resync");
    assert_eq!(marker_names(&inner.project), ["intro"]);
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(marker_names(&inner.project), ["verse"]);

    // Delete.
    let old_project = inner.project.clone();
    apply_remove_marker(&mut inner.project, "verse").unwrap();
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("delete marker", UndoScope::Project),
    )
    .expect("resync");
    assert!(inner.project.markers.is_empty());
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(marker_names(&inner.project), ["verse"]);
}

#[test]
fn two_marker_moves_in_one_gesture_are_one_undo_entry() {
    let mut inner = test_state();
    inner.project = project();
    add(&mut inner.project, "verse", "1:1");

    inner.undo.begin_gesture(UndoScope::Project);
    for at in ["5:1", "9:1"] {
        let old_project = inner.project.clone();
        apply_move_marker(&mut inner.project, "verse", parse_position(at).unwrap()).unwrap();
        resync_with_undo(
            &mut inner,
            old_project,
            UndoEdit::new("move marker", UndoScope::Project),
        )
        .expect("resync");
    }
    inner.undo.end_gesture();

    assert_eq!(inner.undo.undo.len(), 1, "a drag is one history entry");
    assert_eq!(
        inner.project.markers[0].position,
        ast::BarBeat::new(9, 1, 0)
    );
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(
        inner.project.markers[0].position,
        ast::BarBeat::new(1, 1, 0),
        "one undo returns the marker to where the gesture started"
    );
}

#[test]
fn markers_refuse_bad_names_and_positions_with_sentences() {
    let mut project = project();
    add(&mut project, "verse", "1:1");

    let duplicate =
        apply_add_marker(&mut project, "verse", parse_position("9:1").unwrap()).unwrap_err();
    assert!(duplicate.contains("already exists"), "{duplicate}");

    let empty = apply_add_marker(&mut project, "  ", parse_position("9:1").unwrap()).unwrap_err();
    assert!(empty.contains("marker name cannot be empty"), "{empty}");

    let zero = apply_add_marker(&mut project, "zero", ast::BarBeat::new(0, 1, 0)).unwrap_err();
    assert!(zero.contains("bars and beats are 1-based"), "{zero}");

    let subdivision =
        apply_add_marker(&mut project, "fine", ast::BarBeat::new(1, 1, 5)).unwrap_err();
    assert!(subdivision.contains("subdivisions 0-3"), "{subdivision}");

    let unknown = apply_remove_marker(&mut project, "nowhere").unwrap_err();
    assert!(unknown.contains("not found"), "{unknown}");
    assert!(unknown.contains("'verse'"), "{unknown}");

    let rename_taken = apply_rename_marker(&mut project, "verse", "verse").unwrap();
    assert!(!rename_taken, "a rename to the same name is not an edit");
    assert_eq!(marker_names(&project), ["verse"]);
}

#[test]
fn loop_region_is_session_state_that_wraps_at_the_region_end() {
    let state = crate::state::AppState::new();
    let mut inner = state.lock_inner();
    inner.project = project();
    inner.audio.sample_rate = 8000;
    inner.audio.render_thread = Some(crate::render_thread::RenderThreadHandle::spawn(8000));
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();

    let armed = inner
        .audio
        .set_loop_region(Some(1.0), Some(5.0), true, &clock)
        .unwrap();
    assert!(armed.enabled);
    assert_eq!(armed.start_beats, Some(1.0));
    assert_eq!(armed.end_beats, Some(5.0));
    assert!(
        !inner.dsl_source.contains("loop"),
        "a loop region never reaches the DSL"
    );

    // One quarter is half a second at 120 bpm, so 8000 frames per quarter.
    let start = 4000_u64;
    let end = 20_000_u64;
    {
        let rt = inner.audio.render_thread.as_ref().unwrap();
        assert_eq!(
            rt.playback.loop_region(),
            Some(crate::render_thread::LoopRegion { start, end })
        );
        assert_eq!(
            rt.playback.loop_wrap(end - 1, end + 100),
            None,
            "before the end it plays on"
        );
        assert_eq!(rt.playback.loop_wrap(end, end + 100), Some(start));
        assert_eq!(
            rt.playback.loop_wrap(0, end + 100),
            None,
            "a position outside the region is not moved"
        );
    }

    // Loop off plays through: the same cursor keeps playing past the end.
    inner
        .audio
        .set_loop_region(Some(1.0), Some(5.0), false, &clock)
        .unwrap();
    assert!(!inner.audio.loop_state().enabled);
    assert_eq!(
        inner
            .audio
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .loop_wrap(end, end + 100),
        None
    );

    // Clearing the region disarms Loop.
    let cleared = inner
        .audio
        .set_loop_region(None, None, false, &clock)
        .unwrap();
    assert!(!cleared.enabled);
    assert_eq!(cleared.start_beats, None);
    assert_eq!(
        inner
            .audio
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .loop_region(),
        None
    );
}

#[test]
fn a_loop_region_shorter_than_one_beat_is_refused() {
    let state = crate::state::AppState::new();
    let mut inner = state.lock_inner();
    inner.project = project();
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();

    let error = inner
        .audio
        .set_loop_region(Some(1.0), Some(1.5), true, &clock)
        .unwrap_err();
    assert_eq!(
        error,
        "Loop region is shorter than one beat; drag a longer region or clear it."
    );
    assert_eq!(
        inner.audio.loop_state().start_beats,
        None,
        "a refused region is not stored"
    );

    let missing_end = inner
        .audio
        .set_loop_region(Some(1.0), None, true, &clock)
        .unwrap_err();
    assert!(
        missing_end.contains("both a start and an end"),
        "{missing_end}"
    );
}

#[test]
fn a_loop_region_starting_past_the_project_end_is_refused() {
    let state = crate::state::AppState::new();
    let mut inner = state.lock_inner();
    inner.project = project();
    inner.audio.sample_rate = 8000;
    inner.audio.render_thread = Some(crate::render_thread::RenderThreadHandle::spawn(8000));
    {
        let rt = inner.audio.render_thread.as_ref().unwrap();
        // The loaded project would publish this; the loop lives in beats, so
        // the check reads the frame bound the render thread holds.
        rt.playback.total_frames.store(8000, Ordering::Release);
    }
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();

    let error = inner
        .audio
        .set_loop_region(Some(9.0), Some(13.0), true, &clock)
        .unwrap_err();
    assert_eq!(
        error,
        "Loop region starts past the project end; drag it over a bar that has audio."
    );
}

/// The seam the transport wraps at, in frames.
fn loop_seam_frames(inner: &crate::state::AppStateInner) -> Option<(u64, u64)> {
    inner
        .audio
        .render_thread
        .as_ref()
        .and_then(|rt| rt.playback.loop_region())
        .map(|region| (region.start, region.end))
}

/// A region is session state in beats, so a tempo change moves its seam with
/// the bars instead of leaving it at the audio position it was drawn on.
#[test]
fn a_tempo_change_keeps_the_loop_beats_and_moves_the_seam() {
    let state = crate::state::AppState::new();
    let mut inner = state.lock_inner();
    inner.project = project();
    inner.audio.sample_rate = 8000;
    inner.audio.render_thread = Some(crate::render_thread::RenderThreadHandle::spawn(8000));
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
    inner
        .audio
        .set_loop_region(Some(1.0), Some(5.0), true, &clock)
        .unwrap();
    // Four quarters at 120 bpm are 2 s.
    assert_eq!(loop_seam_frames(&inner), Some((4_000, 20_000)));
    let generation = inner
        .audio
        .render_thread
        .as_ref()
        .unwrap()
        .playback
        .loop_generation
        .load(Ordering::Acquire);

    // A tempo map point at bar 1: the same quarters now run at 140 bpm.
    crate::commands::project::set_tempo_map_inner(
        &mut inner,
        vec![ast::TempoPoint {
            position: ast::BarBeat::new(1, 1, 0),
            offset_quarters: 0.0,
            bpm: 140.0,
        }],
    )
    .unwrap();
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
    let expected = (
        clock.sample_at_quarter(1.0, 8000).unwrap(),
        clock.sample_at_quarter(5.0, 8000).unwrap(),
    );
    assert_eq!(expected, (3_428, 17_142));
    assert_eq!(
        loop_seam_frames(&inner),
        Some(expected),
        "the seam follows the bars into the new clock"
    );
    let reported = inner.audio.loop_state();
    assert_eq!(reported.start_beats, Some(1.0));
    assert_eq!(reported.end_beats, Some(5.0));
    assert!(reported.enabled);
    assert!(!reported.clamped, "the project is longer than four bars");

    // The wrap itself lands on the new seam, not on the old audio position.
    let playback = &inner.audio.render_thread.as_ref().unwrap().playback;
    assert_eq!(playback.loop_wrap(17_141, 1 << 40), None);
    assert_eq!(playback.loop_wrap(17_142, 1 << 40), Some(3_428));

    // A moved seam drops the pass queued for the old one, so a live change is
    // heard from where the audio is instead of replaying it.
    assert!(
        playback.loop_generation.load(Ordering::Acquire) > generation,
        "the moved seam invalidates the queued pass"
    );
    let generation = playback.loop_generation.load(Ordering::Acquire);

    // An edit that leaves the clock alone keeps the rolling pass.
    let project = inner.project.clone();
    inner.audio.preload_project(&project);
    assert_eq!(
        inner
            .audio
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .loop_generation
            .load(Ordering::Acquire),
        generation,
        "an unchanged seam is not re-pushed"
    );
}

/// A loop end past the project end stops at the project end, and the transport
/// reports the cut while keeping the beats the user drew.
#[test]
fn a_loop_end_past_the_project_end_is_clamped_and_reported() {
    let state = crate::state::AppState::new();
    let mut inner = state.lock_inner();
    inner.project = project();
    inner.audio.sample_rate = 8000;
    inner.audio.render_thread = Some(crate::render_thread::RenderThreadHandle::spawn(8000));
    // The loaded project would publish this: 1.5 s of audio at 8 kHz.
    let project_end = 12_000_u64;
    inner
        .audio
        .render_thread
        .as_ref()
        .unwrap()
        .playback
        .total_frames
        .store(project_end, Ordering::Release);
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();

    let reported = inner
        .audio
        .set_loop_region(Some(1.0), Some(5.0), true, &clock)
        .unwrap();
    assert!(reported.clamped, "the end past the project end is reported");
    assert_eq!(reported.start_beats, Some(1.0), "the drawn beats are kept");
    assert_eq!(reported.end_beats, Some(5.0));
    assert_eq!(
        inner.audio.active_loop_frames(),
        Some((4_000, project_end)),
        "the pass a take records stops at the project end"
    );

    // A pass wraps where the audio ends, not where the region was drawn.
    let playback = &inner.audio.render_thread.as_ref().unwrap().playback;
    assert_eq!(playback.loop_wrap(project_end, project_end), Some(4_000));
    assert_eq!(playback.loop_wrap(project_end - 1, project_end), None);
}

#[test]
fn a_loop_region_does_not_change_the_offline_render() {
    let state = crate::state::AppState::new();
    let mut inner = state.lock_inner();
    inner.project = project();
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();

    let render = |project: &ast::Project| {
        oc_engine::render::render_to_buffer_with_plugins_at(
            project,
            Path::new(""),
            8000,
            &mut HashMap::new(),
            None,
        )
        .expect("fixture renders")
    };
    let before = render(&inner.project);

    inner
        .audio
        .set_loop_region(Some(1.0), Some(5.0), true, &clock)
        .unwrap();
    assert!(inner.audio.loop_state().enabled);

    let after = render(&inner.project);
    assert_eq!(
        before, after,
        "playback loop state is not an input to the offline renderer"
    );

    let without_markers = oc_dsl::parse(SONG).unwrap();
    assert_eq!(
        oc_dsl::serialize(&inner.project),
        oc_dsl::serialize(&without_markers),
        "arming a loop writes nothing into the project text"
    );
}

#[test]
fn section_repeats_do_not_move_a_loop_region() {
    let state = crate::state::AppState::new();
    let mut inner = state.lock_inner();
    inner.project = oc_dsl::parse(REPEATED).unwrap();
    let rendered = oc_dsl::arrangement::rendered_end_quarter(&inner.project).unwrap();
    assert_eq!(rendered, Some(48.0), "three plays of a four-bar span");

    inner.audio.sample_rate = 8000;
    inner.audio.render_thread = Some(crate::render_thread::RenderThreadHandle::spawn(8000));
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
    inner
        .audio
        .set_loop_region(Some(1.0), Some(5.0), true, &clock)
        .unwrap();

    // The region follows the written quarters the streaming transport plays,
    // not the expanded render: 1..5 quarters is 4000..20000 frames here.
    {
        let rt = inner.audio.render_thread.as_ref().unwrap();
        assert_eq!(
            rt.playback.loop_region(),
            Some(crate::render_thread::LoopRegion {
                start: 4000,
                end: 20000
            })
        );
    }

    // Arming playback state changes neither the arrangement nor its length.
    assert_eq!(
        oc_dsl::arrangement::rendered_end_quarter(&inner.project).unwrap(),
        rendered
    );
}
