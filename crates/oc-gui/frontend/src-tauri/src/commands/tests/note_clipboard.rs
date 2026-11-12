//! Clipboard paste and duplicate: the copied note keeps every field the AST
//! stores, one command is one undo entry, and a refusal happens before the
//! first write.

use oc_dsl::ast::{
    Accidental, Articulation, BaseDuration, Duration, Dynamics, NoteEvent, NoteName, ParamValue,
    Pitch,
};

use crate::commands::notes::{
    NoteInsert, NoteRef, NoteScope, add_notes_at, duplicate_notes_at, placed_note,
};
use crate::state::AppState;

use super::support::{TempDir, state_with_project};

const PATTERN: &str = r#"@ocp "0.2.0"
@project "note clipboard pattern fixture"
  tempo: 120
  time_signature: 4/4
@pattern riff | instrument: piano/acoustic_grand
  events:
    @0 C4 q mf
"#;

const TRACK: &str = r#"@ocp "0.2.0"
@project "note clipboard track fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1 - 3:1]
    @0 C4 q mf
"#;

fn state_from(source: &str, label: &str) -> (AppState, TempDir) {
    let dir = TempDir::new(label);
    let state = state_with_project(source, dir.path());
    (state, dir)
}

fn pattern() -> NoteScope {
    NoteScope::Pattern {
        pattern_name: "riff".to_string(),
    }
}

fn track() -> NoteScope {
    NoteScope::Track { track_index: 0 }
}

/// A quarter-note C4 at `start_beat`, with nothing else written.
fn plain_note(start_beat: f64) -> NoteEvent {
    NoteEvent {
        start_beat,
        explicit_position: true,
        pitch: Pitch {
            note: NoteName::C,
            accidental: Accidental::Natural,
            octave: 4,
        },
        duration: Duration {
            base: BaseDuration::Quarter,
            dots: 0,
            triplet: false,
            tuplet: None,
            ties: vec![],
        },
        dynamics: None,
        velocity: None,
        articulation: None,
        vibrato: None,
        expression: None,
        pitch_bend: None,
        muted: false,
        grace: false,
        text: None,
        modifiers: vec![],
    }
}

/// The note the paste test copies: every field the AST stores is written.
fn decorated_note(start_beat: f64) -> NoteEvent {
    NoteEvent {
        velocity: Some(127),
        dynamics: Some(Dynamics::FFF),
        articulation: Some(Articulation::Staccato),
        muted: true,
        grace: true,
        modifiers: vec![("timing_offset_beats".to_string(), ParamValue::Number(0.125))],
        ..plain_note(start_beat)
    }
}

fn insert(note: NoteEvent) -> NoteInsert {
    NoteInsert {
        phrase_index: 0,
        note,
    }
}

fn refs(indices: &[usize]) -> Vec<NoteRef> {
    indices
        .iter()
        .map(|index| NoteRef {
            phrase_index: 0,
            note_index: *index,
        })
        .collect()
}

fn undo_len(state: &AppState) -> usize {
    state.lock_inner().undo.undo.len()
}

fn pattern_len(state: &AppState) -> usize {
    state.lock_inner().project.patterns[0].events.len()
}

#[test]
fn a_single_add_from_a_copied_note_keeps_its_fields_and_takes_the_start() {
    let event = placed_note(Some(decorated_note(3.5)), 2.0, true, || {
        unreachable!("a copied note supplies its own fields")
    })
    .expect("a copied note is placed");

    assert_eq!(event.start_beat, 2.0);
    assert!(event.explicit_position);
    assert_eq!(event.velocity, Some(127));
    assert_eq!(event.dynamics, Some(Dynamics::FFF));
    assert_eq!(event.articulation, Some(Articulation::Staccato));
    assert!(event.muted);
    assert!(event.grace);
}

#[test]
fn a_single_add_without_a_copied_note_builds_a_fresh_one() {
    let fresh = NoteEvent {
        explicit_position: false,
        ..plain_note(1.0)
    };
    let event = placed_note(None, 1.0, false, || Ok(fresh)).expect("fresh note");

    assert_eq!(event.start_beat, 1.0);
    assert!(!event.explicit_position);
    assert_eq!(event.velocity, None);
}

#[test]
fn a_paste_keeps_every_field_and_one_undo_restores_the_list() {
    let (state, _dir) = state_from(PATTERN, "clipboard-paste-fields");

    add_notes_at(&state, pattern(), &[insert(decorated_note(1.0))]).expect("paste succeeds");

    {
        let inner = state.lock_inner();
        let pasted = &inner.project.patterns[0].events[1];
        assert_eq!(pasted.start_beat, 1.0);
        assert!(pasted.explicit_position);
        assert_eq!(pasted.velocity, Some(127));
        assert_eq!(pasted.dynamics, Some(Dynamics::FFF));
        assert_eq!(pasted.articulation, Some(Articulation::Staccato));
        assert!(pasted.muted);
        assert!(pasted.grace);
        assert_eq!(
            pasted.modifiers,
            vec![("timing_offset_beats".to_string(), ParamValue::Number(0.125))]
        );
        assert_eq!(inner.undo.undo.len(), 1);
    }

    {
        let mut guard = state.lock_inner();
        let inner = &mut *guard;
        let label = inner.undo.undo(&mut inner.project);
        assert_eq!(label.unwrap(), "note paste");
    }
    assert_eq!(pattern_len(&state), 1);
}

#[test]
fn a_paste_of_several_notes_is_one_undo_entry() {
    let (state, _dir) = state_from(PATTERN, "clipboard-paste-many");
    let inserts = vec![
        insert(decorated_note(1.0)),
        insert(plain_note(1.5)),
        insert(plain_note(2.0)),
    ];

    add_notes_at(&state, pattern(), &inserts).expect("paste succeeds");

    assert_eq!(pattern_len(&state), 4);
    assert_eq!(undo_len(&state), 1);

    {
        let mut guard = state.lock_inner();
        let inner = &mut *guard;
        inner.undo.undo(&mut inner.project).expect("one entry");
    }
    assert_eq!(pattern_len(&state), 1);
}

#[test]
fn a_paste_into_a_track_lands_in_the_named_phrase() {
    let (state, _dir) = state_from(TRACK, "clipboard-paste-track");

    add_notes_at(&state, track(), &[insert(decorated_note(1.0))]).expect("paste succeeds");

    let inner = state.lock_inner();
    let events = &inner.project.tracks[0].phrases[0].events;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].velocity, Some(127));
    assert_eq!(events[1].articulation, Some(Articulation::Staccato));
    assert!(events[1].muted);
    assert!(events[1].grace);
}

#[test]
fn an_invalid_batch_pastes_nothing() {
    let (state, _dir) = state_from(PATTERN, "clipboard-paste-invalid");

    let error = add_notes_at(
        &state,
        pattern(),
        &[insert(plain_note(1.0)), insert(plain_note(f64::NAN))],
    )
    .unwrap_err();

    assert_eq!(error, "Non-finite note position");
    assert_eq!(pattern_len(&state), 1);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn a_paste_into_a_missing_container_refuses_before_writing() {
    let (pattern_state, _dir) = state_from(PATTERN, "clipboard-paste-missing");

    let missing = NoteScope::Pattern {
        pattern_name: "nope".to_string(),
    };
    let error = add_notes_at(&pattern_state, missing, &[insert(plain_note(1.0))]).unwrap_err();

    assert_eq!(error, "Pattern 'nope' not found");
    assert_eq!(pattern_len(&pattern_state), 1);
    assert_eq!(undo_len(&pattern_state), 0);

    let (track_state, _dir) = state_from(TRACK, "clipboard-paste-bad-phrase");
    let mut bad_phrase = insert(plain_note(1.0));
    bad_phrase.phrase_index = 4;
    let error = add_notes_at(&track_state, track(), &[bad_phrase]).unwrap_err();
    assert_eq!(error, "Phrase 4 out of range");
    assert_eq!(
        track_state.lock_inner().project.tracks[0].phrases[0]
            .events
            .len(),
        1
    );
}

#[test]
fn an_empty_paste_writes_no_entry() {
    let (state, _dir) = state_from(PATTERN, "clipboard-paste-empty");

    add_notes_at(&state, pattern(), &[]).expect("empty paste is a no-op");

    assert_eq!(pattern_len(&state), 1);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn duplicate_copies_every_field_and_one_undo_removes_the_copy() {
    let (state, _dir) = state_from(PATTERN, "clipboard-duplicate-fields");

    duplicate_notes_at(&state, pattern(), &refs(&[0]), 1.0).expect("duplicate succeeds");

    {
        let inner = state.lock_inner();
        let events = &inner.project.patterns[0].events;
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].start_beat, 1.0);
        assert_eq!(events[1].pitch, events[0].pitch);
        assert_eq!(events[1].duration, events[0].duration);
        assert_eq!(events[1].dynamics, events[0].dynamics);
        assert_eq!(inner.undo.undo.len(), 1);
    }

    {
        let mut guard = state.lock_inner();
        let inner = &mut *guard;
        let label = inner.undo.undo(&mut inner.project);
        assert_eq!(label.unwrap(), "note duplicate");
    }
    assert_eq!(pattern_len(&state), 1);
}

#[test]
fn a_multi_note_duplicate_is_one_undo_entry() {
    let (state, _dir) = state_from(TRACK, "clipboard-duplicate-many");

    duplicate_notes_at(&state, track(), &refs(&[0]), 1.0).expect("duplicate succeeds");

    let inner = state.lock_inner();
    assert_eq!(inner.project.tracks[0].phrases[0].events.len(), 2);
    assert_eq!(inner.project.tracks[0].phrases[0].events[1].start_beat, 1.0);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn duplicate_refuses_a_bad_index_before_writing() {
    let (state, _dir) = state_from(PATTERN, "clipboard-duplicate-refuse");

    let error = duplicate_notes_at(&state, pattern(), &refs(&[0, 9]), 1.0).unwrap_err();

    assert_eq!(error, "Note 9 out of range");
    assert_eq!(pattern_len(&state), 1);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn duplicate_refuses_a_non_finite_offset_before_writing() {
    let (state, _dir) = state_from(PATTERN, "clipboard-duplicate-nan");

    let error = duplicate_notes_at(&state, pattern(), &refs(&[0]), f64::NAN).unwrap_err();

    assert_eq!(error, "Non-finite duplicate offset");
    assert_eq!(pattern_len(&state), 1);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn duplicate_never_writes_a_negative_start() {
    let (state, _dir) = state_from(PATTERN, "clipboard-duplicate-negative");

    duplicate_notes_at(&state, pattern(), &refs(&[0]), -4.0).expect("duplicate succeeds");

    let inner = state.lock_inner();
    assert_eq!(inner.project.patterns[0].events[0].start_beat, 0.0);
    assert_eq!(inner.project.patterns[0].events[1].start_beat, 0.0);
}
