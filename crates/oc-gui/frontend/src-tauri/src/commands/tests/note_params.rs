//! Clearing a note's exact velocity: the written dynamic stays, the edit is one
//! undo entry, and an unchanged note records nothing.

use oc_dsl::ast::Dynamics;

use crate::commands::notes::{clear_note_velocity_at, clear_pattern_note_velocity_at};
use crate::state::AppState;

use super::support::{TempDir, state_with_project};

const PHRASE: &str = r#"@ocp "0.2.0"
@project "velocity fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1 - 5:1]
    C4 q mf v=96
    D4 q mp
"#;

const PATTERN: &str = r#"@ocp "0.2.0"
@project "pattern velocity fixture"
  tempo: 120
  time_signature: 4/4
@pattern riff | instrument: drums/acoustic_kit
  events:
    C2 q mf v=88
"#;

fn phrase_state() -> (AppState, TempDir) {
    let dir = TempDir::new("note-params");
    let state = state_with_project(PHRASE, dir.path());
    (state, dir)
}

fn pattern_state() -> (AppState, TempDir) {
    let dir = TempDir::new("pattern-note-params");
    let state = state_with_project(PATTERN, dir.path());
    (state, dir)
}

fn undo_len(state: &AppState) -> usize {
    state.lock_inner().undo.undo.len()
}

#[test]
fn clearing_an_exact_velocity_leaves_the_written_dynamic() {
    let (state, _dir) = phrase_state();
    let snapshot = clear_note_velocity_at(&state, 0, 0, 0).expect("the velocity clears");

    let note = &snapshot.project.tracks[0].phrases[0].events[0];
    assert_eq!(note.velocity, None);
    assert_eq!(note.dynamics, Some(Dynamics::MF));
    // The serializer writes the token again now that nothing overrides it.
    assert!(
        snapshot.dsl_source.contains("C4 q mf"),
        "{}",
        snapshot.dsl_source
    );
    assert!(
        !snapshot.dsl_source.contains("v=96"),
        "{}",
        snapshot.dsl_source
    );
}

#[test]
fn clearing_a_velocity_records_one_undo_entry_and_only_when_it_changes() {
    let (state, _dir) = phrase_state();
    let before = undo_len(&state);

    clear_note_velocity_at(&state, 0, 0, 0).expect("the velocity clears");
    assert_eq!(undo_len(&state), before + 1);

    // The note already follows its dynamic: no second entry, no error.
    clear_note_velocity_at(&state, 0, 0, 0).expect("clearing again is a no-op");
    assert_eq!(undo_len(&state), before + 1);
}

#[test]
fn a_note_written_without_a_velocity_is_untouched() {
    let (state, _dir) = phrase_state();
    let before = undo_len(&state);

    let snapshot =
        clear_note_velocity_at(&state, 0, 0, 1).expect("a written dynamic needs no clear");
    let note = &snapshot.project.tracks[0].phrases[0].events[1];
    assert_eq!(note.velocity, None);
    assert_eq!(note.dynamics, Some(Dynamics::MP));
    assert_eq!(undo_len(&state), before);
}

#[test]
fn out_of_range_phrase_indices_report_the_existing_message() {
    let (state, _dir) = phrase_state();

    assert_eq!(
        clear_note_velocity_at(&state, 9, 0, 0).unwrap_err(),
        "Track 9 out of range"
    );
    assert_eq!(
        clear_note_velocity_at(&state, 0, 9, 0).unwrap_err(),
        "Phrase 9 out of range"
    );
    assert_eq!(
        clear_note_velocity_at(&state, 0, 0, 9).unwrap_err(),
        "Note 9 out of range"
    );
}

#[test]
fn clearing_a_pattern_note_velocity_leaves_its_dynamic() {
    let (state, _dir) = pattern_state();
    let before = undo_len(&state);

    let snapshot = clear_pattern_note_velocity_at(&state, "riff", 0).expect("the velocity clears");
    let note = &snapshot.project.patterns[0].events[0];
    assert_eq!(note.velocity, None);
    assert_eq!(note.dynamics, Some(Dynamics::MF));
    assert_eq!(undo_len(&state), before + 1);
    assert!(
        snapshot.dsl_source.contains("C2 q mf"),
        "{}",
        snapshot.dsl_source
    );

    // A pattern that does not exist, or a note index past the end, refuses.
    assert_eq!(
        clear_pattern_note_velocity_at(&state, "nope", 0).unwrap_err(),
        "Pattern 'nope' not found"
    );
    assert_eq!(
        clear_pattern_note_velocity_at(&state, "riff", 9).unwrap_err(),
        "Note 9 out of range"
    );
}
