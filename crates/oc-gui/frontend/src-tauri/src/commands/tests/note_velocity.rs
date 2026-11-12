//! Velocity lane tools: ramp, scale and compress edit many notes and record one
//! undo entry. A gesture that would change nothing records nothing.

use crate::commands::notes::{
    NoteRef, NoteScope, compress_note_velocity_at, ramp_note_velocity_at, scale_note_velocity_at,
};
use crate::state::AppState;

use super::support::{TempDir, state_with_project};

/// Three notes on one track, a beat apart, with written velocities at both ends.
const RAMP: &str = r#"@ocp "0.2.0"
@project "velocity fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1 - 5:1]
    @0 C4 q mf v=16
    @1 D4 q mf v=64
    @2 E4 q mf v=120
"#;

/// A note written with a dynamic token only, so its loudness is not an exact value.
const WRITTEN: &str = r#"@ocp "0.2.0"
@project "written dynamic fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1 - 5:1]
    @0 C4 q mp
    @1 D4 q mf
"#;

/// Two notes starting on the same beat: a ramp has no span to walk.
const CHORD: &str = r#"@ocp "0.2.0"
@project "chord fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1 - 5:1]
    @0 C4 q mf v=40
    @0 E4 q mf v=100
"#;

const PATTERN: &str = r#"@ocp "0.2.0"
@project "velocity pattern fixture"
  tempo: 120
  time_signature: 4/4
@pattern riff | instrument: drums/acoustic_kit
  events:
    @0 C2 16th mf v=40
    @1 D2 16th mf v=100
"#;

fn state_from(source: &str, label: &str) -> (AppState, TempDir) {
    let dir = TempDir::new(label);
    let state = state_with_project(source, dir.path());
    (state, dir)
}

fn note(index: usize) -> NoteRef {
    NoteRef {
        phrase_index: 0,
        note_index: index,
    }
}

fn track() -> NoteScope {
    NoteScope::Track { track_index: 0 }
}

fn pattern() -> NoteScope {
    NoteScope::Pattern {
        pattern_name: "riff".to_string(),
    }
}

/// Velocities of the phrase's notes, in their stored order.
fn velocities(state: &AppState) -> Vec<Option<u8>> {
    state.lock_inner().project.tracks[0].phrases[0]
        .events
        .iter()
        .map(|event| event.velocity)
        .collect()
}

fn undo_len(state: &AppState) -> usize {
    state.lock_inner().undo.undo.len()
}

#[test]
fn ramp_walks_from_the_first_selected_velocity_to_the_last() {
    let (state, _dir) = state_from(RAMP, "velocity-ramp");

    ramp_note_velocity_at(&state, track(), &[note(0), note(1), note(2)]).expect("a ramp succeeds");

    // 16 to 120 across two beats puts the middle note halfway: 68.
    assert_eq!(velocities(&state), vec![Some(16), Some(68), Some(120)]);
}

#[test]
fn ramp_orders_the_selection_by_start_and_keeps_the_endpoints() {
    let (state, _dir) = state_from(RAMP, "velocity-ramp-order");

    // The selection arrives out of order; the ramp still runs left to right.
    ramp_note_velocity_at(&state, track(), &[note(2), note(0), note(1)]).expect("a ramp succeeds");

    assert_eq!(velocities(&state), vec![Some(16), Some(68), Some(120)]);
}

#[test]
fn ramp_needs_two_notes() {
    let (state, _dir) = state_from(RAMP, "velocity-ramp-one");
    let undo = undo_len(&state);

    assert_eq!(
        ramp_note_velocity_at(&state, track(), &[note(0)]).unwrap_err(),
        "A ramp needs at least two notes"
    );
    assert_eq!(undo_len(&state), undo);
}

#[test]
fn ramp_needs_notes_at_two_different_starts() {
    let (state, _dir) = state_from(CHORD, "velocity-ramp-chord");
    let undo = undo_len(&state);

    assert_eq!(
        ramp_note_velocity_at(&state, track(), &[note(0), note(1)]).unwrap_err(),
        "A ramp needs notes at two different starts"
    );
    assert_eq!(undo_len(&state), undo);
}

#[test]
fn scale_halves_the_distance_to_zero_at_fifty_percent() {
    let (state, _dir) = state_from(RAMP, "velocity-scale-half");

    scale_note_velocity_at(&state, track(), &[note(0), note(1), note(2)], 50.0)
        .expect("a scale succeeds");

    // Scaling multiplies the velocity itself, so 50 % halves the distance to 0.
    assert_eq!(velocities(&state), vec![Some(8), Some(32), Some(60)]);
}

#[test]
fn scale_writes_an_exact_value_for_a_written_dynamic() {
    let (state, _dir) = state_from(WRITTEN, "velocity-scale-written");

    scale_note_velocity_at(&state, track(), &[note(1)], 50.0).expect("a scale succeeds");

    // mf is 80; half of it is 40, and the token stays written as its origin.
    assert_eq!(velocities(&state), vec![None, Some(40)]);
    assert_eq!(
        state.lock_inner().project.tracks[0].phrases[0].events[1].dynamics,
        Some(oc_dsl::ast::Dynamics::MF)
    );
}

#[test]
fn scale_refuses_a_percentage_outside_the_range() {
    let (state, _dir) = state_from(RAMP, "velocity-scale-refuse");
    let undo = undo_len(&state);

    assert_eq!(
        scale_note_velocity_at(&state, track(), &[note(0)], 0.0).unwrap_err(),
        "Scale must be a percentage in (0, 1000]"
    );
    assert_eq!(undo_len(&state), undo);
}

#[test]
fn a_scale_that_changes_nothing_records_no_undo() {
    let (state, _dir) = state_from(RAMP, "velocity-scale-noop");
    let before = velocities(&state);
    let undo = undo_len(&state);

    scale_note_velocity_at(&state, track(), &[note(0), note(1), note(2)], 100.0)
        .expect("a 100 % scale succeeds");

    assert_eq!(velocities(&state), before);
    assert_eq!(undo_len(&state), undo);
}

#[test]
fn compress_moves_halfway_toward_the_target_and_leaves_a_note_at_it_alone() {
    let (state, _dir) = state_from(RAMP, "velocity-compress");

    compress_note_velocity_at(&state, track(), &[note(0), note(1), note(2)], 64)
        .expect("a compress succeeds");

    // 16 -> 40 is halfway to 64; 64 stays put; 120 -> 92.
    assert_eq!(velocities(&state), vec![Some(40), Some(64), Some(92)]);
}

#[test]
fn compress_refuses_a_target_outside_the_midi_range() {
    let (state, _dir) = state_from(RAMP, "velocity-compress-refuse");
    let undo = undo_len(&state);

    assert_eq!(
        compress_note_velocity_at(&state, track(), &[note(0)], 0).unwrap_err(),
        "Compress target must be 1-127"
    );
    assert_eq!(undo_len(&state), undo);
}

#[test]
fn each_velocity_tool_records_one_undo_entry() {
    let (ramped, _ramp_dir) = state_from(RAMP, "velocity-undo-ramp");
    let before = undo_len(&ramped);
    ramp_note_velocity_at(&ramped, track(), &[note(0), note(1), note(2)]).expect("a ramp succeeds");
    assert_eq!(undo_len(&ramped), before + 1);

    let (scaled, _scale_dir) = state_from(RAMP, "velocity-undo-scale");
    let before = undo_len(&scaled);
    scale_note_velocity_at(&scaled, track(), &[note(0), note(2)], 50.0).expect("a scale succeeds");
    assert_eq!(undo_len(&scaled), before + 1);

    let (compressed, _compress_dir) = state_from(RAMP, "velocity-undo-compress");
    let before = undo_len(&compressed);
    compress_note_velocity_at(&compressed, track(), &[note(0), note(2)], 64)
        .expect("a compress succeeds");
    assert_eq!(undo_len(&compressed), before + 1);
}

#[test]
fn a_pattern_gesture_reaches_pattern_notes() {
    let (state, _dir) = state_from(PATTERN, "velocity-pattern");

    ramp_note_velocity_at(&state, pattern(), &[note(0), note(1)]).expect("a ramp succeeds");

    assert_eq!(
        state.lock_inner().project.patterns[0]
            .events
            .iter()
            .map(|event| event.velocity)
            .collect::<Vec<_>>(),
        vec![Some(40), Some(100)]
    );
}

#[test]
fn a_track_gesture_covers_every_phrase_in_one_entry() {
    const TWO_PHRASES: &str = r#"@ocp "0.2.0"
@project "velocity two phrase fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase first [1:1 - 3:1]
    @0 C4 q mf v=40
    @4 D4 q mf v=40
  @phrase second [3:1 - 5:1]
    @0 E4 q mf v=120
"#;
    let (state, _dir) = state_from(TWO_PHRASES, "velocity-two-phrases");
    let undo = undo_len(&state);
    let selected = [
        note(0),
        note(1),
        NoteRef {
            phrase_index: 1,
            note_index: 0,
        },
    ];

    ramp_note_velocity_at(&state, track(), &selected).expect("a ramp succeeds");

    // The second note sits four quarters in, halfway between the endpoints, so
    // the ramp has to see the phrases' own offsets to place it.
    let inner = state.lock_inner();
    assert_eq!(
        inner.project.tracks[0].phrases[0].events[0].velocity,
        Some(40)
    );
    assert_eq!(
        inner.project.tracks[0].phrases[0].events[1].velocity,
        Some(80)
    );
    assert_eq!(
        inner.project.tracks[0].phrases[1].events[0].velocity,
        Some(120)
    );
    assert_eq!(inner.undo.undo.len(), undo + 1);
}

#[test]
fn an_empty_selection_is_a_no_op_for_scale_and_compress() {
    let (state, _dir) = state_from(RAMP, "velocity-empty");
    let before = velocities(&state);
    let undo = undo_len(&state);

    scale_note_velocity_at(&state, track(), &[], 50.0).expect("an empty scale is a no-op");
    compress_note_velocity_at(&state, track(), &[], 64).expect("an empty compress is a no-op");

    assert_eq!(velocities(&state), before);
    assert_eq!(undo_len(&state), undo);
}

#[test]
fn a_ramp_without_two_notes_is_refused_and_changes_nothing() {
    let (state, _dir) = state_from(RAMP, "velocity-empty-ramp");
    let before = velocities(&state);
    let undo = undo_len(&state);

    // The roll disables the control; the command still refuses on its own.
    assert_eq!(
        ramp_note_velocity_at(&state, track(), &[]).unwrap_err(),
        "A ramp needs at least two notes"
    );

    assert_eq!(velocities(&state), before);
    assert_eq!(undo_len(&state), undo);
}
