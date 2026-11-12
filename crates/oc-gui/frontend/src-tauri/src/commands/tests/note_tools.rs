//! Batch note tools: quantise, humanise and legato edit many notes and record
//! one undo entry. An empty selection changes nothing and records nothing.

use oc_dsl::ast::NoteName;

use crate::commands::notes::{
    NoteRef, NoteScope, humanise_notes_at, legato_notes_at, quantise_notes_at,
};
use crate::state::AppState;

use super::support::{TempDir, state_with_project};

/// Three notes a 1/4 grid moves, inside a four-bar phrase.
const PHRASE: &str = r#"@ocp "0.2.0"
@project "batch tools fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1 - 5:1]
    @0.13 C4 q mf
    @0.63 D4 q mf
    @1.37 E4 q mf
"#;

/// Short notes with room between them, so legato has somewhere to reach.
const SHORT: &str = r#"@ocp "0.2.0"
@project "legato fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1 - 5:1]
    @0 C4 16th mf
    @1 D4 16th mf
    @2 E4 16th mf
"#;

/// One note near the phrase end, where an amount has to stop.
const LAST: &str = r#"@ocp "0.2.0"
@project "legato end fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1 - 5:1]
    @13 C4 16th mf
"#;

/// A phrase half a quarter long, shorter than a 1/4 grid step.
const SHORT_PHRASE: &str = r#"@ocp "0.2.0"
@project "short phrase fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1:0 - 1:1:2]
    @0.1 C4 16th mf
"#;

/// Two phrases on one track, so one gesture has to cover both.
const TWO_PHRASES: &str = r#"@ocp "0.2.0"
@project "two phrase fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase first [1:1 - 3:1]
    @0.13 C4 q mf
  @phrase second [3:1 - 5:1]
    @0.17 D4 q mf
"#;

/// A phrase two bars long, with a note that reaches its end.
const AT_END: &str = r#"@ocp "0.2.0"
@project "phrase end fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1 - 2:1]
    @3.9 C4 q mf
"#;

const PATTERN: &str = r#"@ocp "0.2.0"
@project "batch pattern fixture"
  tempo: 120
  time_signature: 4/4
@pattern riff | instrument: drums/acoustic_kit
  events:
    @0.13 C2 16th mf
    @0.63 D2 16th mf
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

fn phrase_events(state: &AppState, phrase_index: usize) -> Vec<(f64, f64)> {
    state.lock_inner().project.tracks[0].phrases[phrase_index]
        .events
        .iter()
        .map(|event| (event.start_beat, event.duration.to_beats()))
        .collect()
}

fn pattern_events(state: &AppState) -> Vec<(f64, f64)> {
    state.lock_inner().project.patterns[0]
        .events
        .iter()
        .map(|event| (event.start_beat, event.duration.to_beats()))
        .collect()
}

fn starts(state: &AppState, phrase_index: usize) -> Vec<f64> {
    phrase_events(state, phrase_index)
        .into_iter()
        .map(|(start, _)| start)
        .collect()
}

fn undo_len(state: &AppState) -> usize {
    state.lock_inner().undo.undo.len()
}

#[test]
fn quantise_at_zero_strength_changes_nothing_and_records_no_undo() {
    let (state, _dir) = state_from(PHRASE, "quantise-zero");
    let before = starts(&state, 0);
    let undo = undo_len(&state);

    quantise_notes_at(&state, track(), &[note(0), note(1), note(2)], 0.25, 0.0)
        .expect("a zero-strength quantise succeeds");

    assert_eq!(starts(&state, 0), before);
    assert_eq!(undo_len(&state), undo);
}

#[test]
fn quantise_at_full_strength_lands_on_the_grid() {
    let (state, _dir) = state_from(PHRASE, "quantise-full");

    quantise_notes_at(&state, track(), &[note(0), note(1), note(2)], 0.25, 100.0)
        .expect("a full-strength quantise succeeds");

    assert_eq!(starts(&state, 0), vec![0.25, 0.75, 1.25]);
    // Duration and pitch survive: quantise moves starts only.
    let events = phrase_events(&state, 0);
    assert!(events.iter().all(|(_, duration)| *duration == 1.0));
    let inner = state.lock_inner();
    assert_eq!(
        inner.project.tracks[0].phrases[0].events[0].pitch.note,
        NoteName::C
    );
}

#[test]
fn quantise_strength_moves_a_fraction_of_the_distance() {
    let (state, _dir) = state_from(PHRASE, "quantise-half");

    quantise_notes_at(&state, track(), &[note(0)], 0.25, 50.0)
        .expect("a half-strength quantise succeeds");

    // 0.13 sits 0.12 short of 0.25, so half the distance is 0.19.
    assert!((starts(&state, 0)[0] - 0.19).abs() < 1e-9);
}

#[test]
fn quantise_refuses_a_container_shorter_than_one_grid_step() {
    let (state, _dir) = state_from(SHORT_PHRASE, "quantise-short");
    let before = starts(&state, 0);
    let undo = undo_len(&state);

    assert_eq!(
        quantise_notes_at(&state, track(), &[note(0)], 1.0, 100.0).unwrap_err(),
        "The pattern is shorter than one grid step."
    );
    assert_eq!(starts(&state, 0), before);
    assert_eq!(undo_len(&state), undo);
}

#[test]
fn humanise_is_deterministic_for_the_same_seed() {
    let (first, _first_dir) = state_from(PHRASE, "humanise-seed-a");
    let (second, _second_dir) = state_from(PHRASE, "humanise-seed-b");

    humanise_notes_at(&first, track(), &[note(0), note(1), note(2)], 2, 12, 7)
        .expect("humanise succeeds");
    humanise_notes_at(&second, track(), &[note(0), note(1), note(2)], 2, 12, 7)
        .expect("humanise succeeds");

    assert_eq!(starts(&first, 0), starts(&second, 0));
    assert_eq!(
        phrase_events(&first, 0),
        phrase_events(&second, 0),
        "durations are untouched too"
    );
}

#[test]
fn humanise_bounds_its_timing_and_velocity_offsets() {
    let (state, _dir) = state_from(PHRASE, "humanise-bounds");
    let before = phrase_events(&state, 0);
    let velocities: Vec<Option<u8>> = state.lock_inner().project.tracks[0].phrases[0]
        .events
        .iter()
        .map(|event| event.velocity)
        .collect();

    // Seed 13 moves all three notes on both axes, so the bounds are not vacuous.
    humanise_notes_at(&state, track(), &[note(0), note(1), note(2)], 2, 12, 13)
        .expect("humanise succeeds");

    let after = phrase_events(&state, 0);
    for ((start, duration), (new_start, new_duration)) in before.iter().zip(&after) {
        assert!(
            (new_start - start).abs() <= 2.0 / 48.0 + 1e-9,
            "{new_start} moved more than two editor ticks from {start}"
        );
        assert_eq!(
            duration, new_duration,
            "humanise keeps the written duration"
        );
    }
    let inner = state.lock_inner();
    let mut written = 0;
    for (index, event) in inner.project.tracks[0].phrases[0].events.iter().enumerate() {
        let base = velocities[index].unwrap_or(80);
        let Some(velocity) = event.velocity else {
            continue;
        };
        written += 1;
        assert!(
            (i16::from(velocity) - i16::from(base)).abs() <= 12,
            "{velocity} is more than 12 from {base}"
        );
    }
    assert_eq!(written, 3, "every selected note takes a humanised velocity");
    assert!(
        after
            .iter()
            .zip(&before)
            .any(|((new, _), (old, _))| (new - old).abs() > 1e-9),
        "at least one note moved"
    );
}

#[test]
fn humanise_uses_the_seed_it_is_given() {
    let (first, _first_dir) = state_from(PHRASE, "humanise-seed-1");
    let (second, _second_dir) = state_from(PHRASE, "humanise-seed-2");

    humanise_notes_at(&first, track(), &[note(0), note(1), note(2)], 6, 0, 1)
        .expect("humanise succeeds");
    humanise_notes_at(&second, track(), &[note(0), note(1), note(2)], 6, 0, 2)
        .expect("humanise succeeds");

    assert_ne!(starts(&first, 0), starts(&second, 0));
}

#[test]
fn legato_reaches_the_next_note_and_stops_there() {
    let (state, _dir) = state_from(SHORT, "legato-next");

    legato_notes_at(&state, track(), &[note(0)], None).expect("legato succeeds");

    let events = phrase_events(&state, 0);
    assert_eq!(
        events[0].1, 1.0,
        "the first note ends where the next starts"
    );
    assert_eq!(events[1].1, 0.25, "an unselected note keeps its duration");
    assert_eq!(events[2].1, 0.25, "a note with no successor keeps its end");
}

#[test]
fn legato_clamps_at_the_next_note() {
    let (state, _dir) = state_from(SHORT, "legato-amount-next");

    // Four beats asked for, one beat available before the next note.
    legato_notes_at(&state, track(), &[note(0)], Some(4.0)).expect("legato succeeds");

    assert_eq!(phrase_events(&state, 0)[0].1, 1.0);
}

#[test]
fn legato_clamps_at_the_phrase_end() {
    let (state, _dir) = state_from(LAST, "legato-end");

    legato_notes_at(&state, track(), &[note(0)], Some(8.0)).expect("legato succeeds");

    // The note starts at 13 and the phrase ends at 16, so it grows by 3.
    assert_eq!(phrase_events(&state, 0)[0], (13.0, 3.0));
}

#[test]
fn legato_extends_by_the_stated_amount() {
    let (state, _dir) = state_from(SHORT, "legato-amount");

    legato_notes_at(&state, track(), &[note(0)], Some(0.5)).expect("legato succeeds");

    assert_eq!(phrase_events(&state, 0)[0], (0.0, 0.5));
}

#[test]
fn each_batch_tool_records_one_undo_entry() {
    let (quantised, _quantise_dir) = state_from(PHRASE, "undo-quantise");
    let before = undo_len(&quantised);
    quantise_notes_at(
        &quantised,
        track(),
        &[note(0), note(1), note(2)],
        0.25,
        100.0,
    )
    .expect("quantise succeeds");
    assert_eq!(undo_len(&quantised), before + 1);

    let (humanised, _humanise_dir) = state_from(PHRASE, "undo-humanise");
    let before = undo_len(&humanised);
    humanise_notes_at(&humanised, track(), &[note(0), note(1), note(2)], 2, 12, 7)
        .expect("humanise succeeds");
    assert_eq!(undo_len(&humanised), before + 1);

    let (lengthened, _legato_dir) = state_from(SHORT, "undo-legato");
    let before = undo_len(&lengthened);
    legato_notes_at(&lengthened, track(), &[note(0), note(1)], None).expect("legato succeeds");
    assert_eq!(undo_len(&lengthened), before + 1);
}

#[test]
fn quantise_clamps_a_start_inside_the_phrase_end() {
    let (state, _dir) = state_from(AT_END, "quantise-end");

    // The nearest grid point is the phrase end, which is not a legal start.
    quantise_notes_at(&state, track(), &[note(0)], 1.0, 100.0).expect("quantise succeeds");

    assert!((starts(&state, 0)[0] - (4.0 - 1.0 / 48.0)).abs() < 1e-9);
}

#[test]
fn humanise_clamps_a_start_at_the_phrase_end() {
    let (state, _dir) = state_from(AT_END, "humanise-end");
    let before = starts(&state, 0)[0];

    // Seed 14 pushes this note two ticks later, so the phrase end holds it back.
    humanise_notes_at(&state, track(), &[note(0)], 2, 0, 14).expect("humanise succeeds");

    assert_eq!(
        starts(&state, 0)[0],
        4.0 - 1.0,
        "the note still fits the phrase"
    );
    assert!(
        starts(&state, 0)[0] < before,
        "a forward nudge was held back"
    );
}

#[test]
fn an_empty_selection_changes_nothing_and_records_no_undo() {
    let (state, _dir) = state_from(PHRASE, "batch-empty");
    let before = phrase_events(&state, 0);
    let undo = undo_len(&state);

    quantise_notes_at(&state, track(), &[], 0.25, 100.0).expect("quantise succeeds");
    humanise_notes_at(&state, track(), &[], 2, 12, 7).expect("humanise succeeds");
    legato_notes_at(&state, track(), &[], None).expect("legato succeeds");

    assert_eq!(phrase_events(&state, 0), before);
    assert_eq!(undo_len(&state), undo);
}

#[test]
fn a_track_gesture_covers_every_phrase_in_one_entry() {
    let (state, _dir) = state_from(TWO_PHRASES, "batch-two-phrases");
    let undo = undo_len(&state);
    let selected = [
        note(0),
        NoteRef {
            phrase_index: 1,
            note_index: 0,
        },
    ];

    quantise_notes_at(&state, track(), &selected, 0.25, 100.0).expect("quantise succeeds");

    assert_eq!(starts(&state, 0), vec![0.25]);
    assert_eq!(starts(&state, 1), vec![0.25]);
    assert_eq!(undo_len(&state), undo + 1);
}

#[test]
fn a_pattern_gesture_reaches_pattern_notes() {
    let (state, _dir) = state_from(PATTERN, "batch-pattern");

    quantise_notes_at(&state, pattern(), &[note(0), note(1)], 0.25, 100.0)
        .expect("quantise succeeds");
    assert_eq!(
        pattern_events(&state)
            .into_iter()
            .map(|(start, _)| start)
            .collect::<Vec<_>>(),
        vec![0.25, 0.75]
    );

    legato_notes_at(&state, pattern(), &[note(0)], None).expect("legato succeeds");
    assert_eq!(pattern_events(&state)[0].1, 0.5);
}

#[test]
fn the_payload_the_roll_sends_deserializes() {
    // The keys the roll's `api.quantiseNotes` call sends; Tauri only renames the
    // command's own arguments, so these nested names are serde's.
    let scope: NoteScope = serde_json::from_value(serde_json::json!({
        "kind": "pattern",
        "pattern_name": "beat",
    }))
    .expect("a pattern scope");
    assert!(
        matches!(scope, NoteScope::Pattern { ref pattern_name } if pattern_name == "beat"),
        "{scope:?}"
    );

    let scope: NoteScope = serde_json::from_value(serde_json::json!({
        "kind": "track",
        "track_index": 2,
    }))
    .expect("a track scope");
    assert!(matches!(scope, NoteScope::Track { track_index: 2 }));

    let note: NoteRef = serde_json::from_value(serde_json::json!({
        "phrase_index": 1,
        "note_index": 0,
    }))
    .expect("a note ref");
    assert_eq!((note.phrase_index, note.note_index), (1, 0));

    // A pattern scope ignores the phrase index, so the field may be absent.
    let note: NoteRef = serde_json::from_value(serde_json::json!({ "note_index": 3 }))
        .expect("a note ref without a phrase");
    assert_eq!((note.phrase_index, note.note_index), (0, 3));
}

#[test]
fn out_of_range_references_report_the_existing_message() {
    let (state, _dir) = state_from(PHRASE, "batch-ranges");
    let far_phrase = [NoteRef {
        phrase_index: 9,
        note_index: 0,
    }];

    assert_eq!(
        quantise_notes_at(&state, track(), &far_phrase, 0.25, 100.0).unwrap_err(),
        "Phrase 9 out of range"
    );
    assert_eq!(
        humanise_notes_at(&state, track(), &[note(9)], 2, 12, 7).unwrap_err(),
        "Note 9 out of range"
    );
    assert_eq!(
        legato_notes_at(&state, track(), &[note(9)], None).unwrap_err(),
        "Note 9 out of range"
    );
    assert_eq!(
        quantise_notes_at(
            &state,
            NoteScope::Pattern {
                pattern_name: "nope".to_string()
            },
            &[note(0)],
            0.25,
            100.0
        )
        .unwrap_err(),
        "Pattern 'nope' not found"
    );
    assert_eq!(
        quantise_notes_at(&state, track(), &[note(0)], 0.0, 100.0).unwrap_err(),
        "Grid must be a positive number of quarters"
    );
    assert_eq!(
        legato_notes_at(&state, track(), &[note(0)], Some(0.0)).unwrap_err(),
        "Legato amount must be finite and within (0, 1024] quarters"
    );
}
