//! Batch note editing: move, remove and velocity write every selected note in
//! one command, resolve every index before the first write, and record one undo
//! entry for the whole gesture.

use crate::commands::notes::{
    HitExtras, NoteMove, NoteRef, NoteScope, NoteVelocity, add_pattern_hit_at, add_pattern_note_at,
    move_notes_at, remove_notes_at, remove_pattern_hit_at, set_note_velocities_at,
    set_pattern_bars_at, set_pattern_swing_inner, set_project_swing_subdivision_inner,
    trim_pattern_to_bars_at,
};
use crate::state::AppState;

use super::support::{TempDir, state_with_project};

/// Four sixteenths a step apart, so a one-beat move cannot collapse two notes.
const PHRASE: &str = r#"@ocp "0.2.0"
@project "note selection fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1 - 5:1]
    @0 C4 16th mf
    @0.25 D4 16th mf
    @0.5 E4 16th mf
    @0.75 F4 16th mf
"#;

/// Two phrases on one track: the roll's track view flattens them, so one
/// gesture has to cover both.
const TWO_PHRASES: &str = r#"@ocp "0.2.0"
@project "note selection two phrase fixture"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase first [1:1 - 3:1]
    @0 C4 q mf
    @1 D4 q mf
  @phrase second [3:1 - 5:1]
    @0 E4 q mf
"#;

const PATTERN: &str = r#"@ocp "0.2.0"
@project "note selection pattern fixture"
  tempo: 120
  time_signature: 4/4
@pattern riff | instrument: piano/acoustic_grand
  events:
    @0 C4 16th mf
    @0.25 D4 16th mf
    @0.5 E4 16th mf
    @0.75 F4 16th mf
"#;

/// A two-bar pattern with a note in the second bar, so shortening it has
/// something past the new end to keep or trim.
const LONG_PATTERN: &str = r#"@ocp "0.2.0"
@project "pattern bounds fixture"
  tempo: 120
  time_signature: 4/4
@pattern long | instrument: piano/acoustic_grand
  events:
    @0 C4 q mf
    @4 G4 q mf
"#;

fn state_from(source: &str, label: &str) -> (AppState, TempDir) {
    let dir = TempDir::new(label);
    let state = state_with_project(source, dir.path());
    (state, dir)
}

fn track() -> NoteScope {
    NoteScope::Track { track_index: 0 }
}

fn pattern() -> NoteScope {
    NoteScope::Pattern {
        pattern_name: "riff".to_string(),
    }
}

fn starts(state: &AppState) -> Vec<f64> {
    state.lock_inner().project.tracks[0].phrases[0]
        .events
        .iter()
        .map(|event| event.start_beat)
        .collect()
}

fn pattern_starts(state: &AppState) -> Vec<f64> {
    state.lock_inner().project.patterns[0]
        .events
        .iter()
        .map(|event| event.start_beat)
        .collect()
}

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

/// Index plus target start, the shape the roll sends for a nudge.
fn moves(indices: &[usize], start_of: impl Fn(usize) -> f64) -> Vec<NoteMove> {
    indices
        .iter()
        .map(|index| NoteMove {
            phrase_index: 0,
            note_index: *index,
            new_start_beat: start_of(*index),
            new_pitch: None,
        })
        .collect()
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

#[test]
fn batch_move_moves_every_note_by_the_step_and_records_one_entry() {
    let (state, _dir) = state_from(PHRASE, "batch-move");
    let targets = moves(&[0, 1, 2, 3], |index| index as f64 * 0.25 + 1.0);

    move_notes_at(&state, track(), &targets).expect("batch move succeeds");

    assert_eq!(starts(&state), vec![1.0, 1.25, 1.5, 1.75]);
    assert_eq!(undo_len(&state), 1);
}

#[test]
fn batch_move_is_ordered_by_target_not_by_the_pre_edit_list() {
    let (state, _dir) = state_from(PHRASE, "batch-move-order");
    // A selection walked in reverse: index 3 first, index 0 last. Every target
    // comes from the pre-edit list, so the order of the list cannot move a note
    // twice or leave one behind.
    let targets = moves(&[3, 2, 1, 0], |index| index as f64 * 0.25 + 1.0);

    move_notes_at(&state, track(), &targets).expect("batch move succeeds");

    assert_eq!(starts(&state), vec![1.0, 1.25, 1.5, 1.75]);
}

#[test]
fn batch_move_refuses_a_bad_index_before_any_write() {
    let (state, _dir) = state_from(PHRASE, "batch-move-refuse");
    let targets = moves(&[0, 1, 9], |index| index as f64 * 0.25 + 1.0);

    let error = move_notes_at(&state, track(), &targets).unwrap_err();

    assert_eq!(error, "Note 9 out of range");
    assert_eq!(starts(&state), vec![0.0, 0.25, 0.5, 0.75]);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn batch_move_refuses_a_non_finite_target_before_any_write() {
    let (state, _dir) = state_from(PHRASE, "batch-move-nan");
    let targets = vec![
        NoteMove {
            phrase_index: 0,
            note_index: 0,
            new_start_beat: 1.0,
            new_pitch: None,
        },
        NoteMove {
            phrase_index: 0,
            note_index: 1,
            new_start_beat: f64::NAN,
            new_pitch: None,
        },
    ];

    move_notes_at(&state, track(), &targets).unwrap_err();

    assert_eq!(starts(&state), vec![0.0, 0.25, 0.5, 0.75]);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn batch_move_refuses_a_phrase_the_track_does_not_have() {
    let (state, _dir) = state_from(PHRASE, "batch-move-phrase");
    let mut targets = moves(&[0], |_| 1.0);
    targets[0].phrase_index = 4;

    let error = move_notes_at(&state, track(), &targets).unwrap_err();

    assert_eq!(error, "Phrase 4 out of range");
    assert_eq!(starts(&state), vec![0.0, 0.25, 0.5, 0.75]);
}

#[test]
fn batch_remove_removes_only_the_selection_and_one_undo_restores_all() {
    let (state, _dir) = state_from(PHRASE, "batch-remove");
    let selected = refs(&[0, 2]);

    remove_notes_at(&state, track(), &selected).expect("batch remove succeeds");

    assert_eq!(starts(&state), vec![0.25, 0.75]);
    assert_eq!(undo_len(&state), 1);

    {
        let mut guard = state.lock_inner();
        let inner = &mut *guard;
        let label = inner.undo.undo(&mut inner.project);
        assert_eq!(label.unwrap(), "note remove");
    }
    assert_eq!(starts(&state), vec![0.0, 0.25, 0.5, 0.75]);
}

#[test]
fn batch_remove_removes_all_four_in_one_entry() {
    let (state, _dir) = state_from(PHRASE, "batch-remove-all");

    remove_notes_at(&state, track(), &refs(&[0, 1, 2, 3])).expect("batch remove succeeds");

    assert!(starts(&state).is_empty());
    assert_eq!(undo_len(&state), 1);

    {
        let mut guard = state.lock_inner();
        let inner = &mut *guard;
        inner.undo.undo(&mut inner.project).expect("one undo entry");
    }
    assert_eq!(starts(&state), vec![0.0, 0.25, 0.5, 0.75]);
}

#[test]
fn batch_remove_refuses_a_bad_index_before_removing_anything() {
    let (state, _dir) = state_from(PHRASE, "batch-remove-refuse");

    let error = remove_notes_at(&state, track(), &refs(&[0, 9])).unwrap_err();

    assert_eq!(error, "Note 9 out of range");
    assert_eq!(starts(&state), vec![0.0, 0.25, 0.5, 0.75]);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn batch_remove_writes_one_entry_for_a_repeated_index() {
    let (state, _dir) = state_from(PHRASE, "batch-remove-duplicate");

    remove_notes_at(&state, track(), &refs(&[1, 1])).expect("batch remove succeeds");

    assert_eq!(starts(&state), vec![0.0, 0.5, 0.75]);
    assert_eq!(undo_len(&state), 1);
}

#[test]
fn batch_remove_covers_notes_from_two_phrases_in_one_entry() {
    let (state, _dir) = state_from(TWO_PHRASES, "batch-remove-phrases");
    let selected = vec![
        NoteRef {
            phrase_index: 0,
            note_index: 1,
        },
        NoteRef {
            phrase_index: 1,
            note_index: 0,
        },
    ];

    remove_notes_at(&state, track(), &selected).expect("batch remove succeeds");

    let inner = state.lock_inner();
    assert_eq!(inner.project.tracks[0].phrases[0].events.len(), 1);
    assert_eq!(inner.project.tracks[0].phrases[1].events.len(), 0);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn batch_move_covers_notes_from_two_phrases() {
    let (state, _dir) = state_from(TWO_PHRASES, "batch-move-phrases");
    let targets = vec![
        NoteMove {
            phrase_index: 0,
            note_index: 1,
            new_start_beat: 2.0,
            new_pitch: None,
        },
        NoteMove {
            phrase_index: 1,
            note_index: 0,
            new_start_beat: 1.0,
            new_pitch: None,
        },
    ];

    move_notes_at(&state, track(), &targets).expect("batch move succeeds");

    let inner = state.lock_inner();
    assert_eq!(inner.project.tracks[0].phrases[0].events[1].start_beat, 2.0);
    assert_eq!(inner.project.tracks[0].phrases[1].events[0].start_beat, 1.0);
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn batch_velocity_writes_every_selected_note_in_one_entry() {
    let (state, _dir) = state_from(PHRASE, "batch-velocity");
    let values = vec![
        NoteVelocity {
            phrase_index: 0,
            note_index: 0,
            velocity: 112,
        },
        NoteVelocity {
            phrase_index: 0,
            note_index: 2,
            velocity: 33,
        },
    ];

    set_note_velocities_at(&state, track(), &values).expect("batch velocity succeeds");

    assert_eq!(velocities(&state), vec![Some(112), None, Some(33), None]);
    assert_eq!(undo_len(&state), 1);
}

#[test]
fn batch_velocity_refuses_a_bad_index_and_leaves_every_velocity_unchanged() {
    let (state, _dir) = state_from(PHRASE, "batch-velocity-refuse");
    let values = vec![
        NoteVelocity {
            phrase_index: 0,
            note_index: 0,
            velocity: 112,
        },
        NoteVelocity {
            phrase_index: 0,
            note_index: 9,
            velocity: 40,
        },
    ];

    let error = set_note_velocities_at(&state, track(), &values).unwrap_err();

    assert_eq!(error, "Note 9 out of range");
    // The first value is never applied: the refusal happens before the write.
    assert_eq!(velocities(&state), vec![None, None, None, None]);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn batch_move_handles_a_pattern_scope_and_sorts_once() {
    let (state, _dir) = state_from(PATTERN, "batch-pattern-move");
    let targets = moves(&[0, 1, 2, 3], |index| index as f64 * 0.25 + 1.0);

    move_notes_at(&state, pattern(), &targets).expect("batch move succeeds");

    assert_eq!(pattern_starts(&state), vec![1.0, 1.25, 1.5, 1.75]);
    assert_eq!(undo_len(&state), 1);
}

#[test]
fn batch_pattern_remove_and_velocity_stay_one_entry_each() {
    let (state, _dir) = state_from(PATTERN, "batch-pattern-remove");

    remove_notes_at(&state, pattern(), &refs(&[0, 1])).expect("batch remove succeeds");
    assert_eq!(pattern_starts(&state), vec![0.5, 0.75]);
    assert_eq!(undo_len(&state), 1);

    let values = vec![NoteVelocity {
        phrase_index: 0,
        note_index: 0,
        velocity: 64,
    }];
    set_note_velocities_at(&state, pattern(), &values).expect("batch velocity succeeds");
    assert_eq!(
        state.lock_inner().project.patterns[0].events[0].velocity,
        Some(64)
    );
    assert_eq!(undo_len(&state), 2);
}

#[test]
fn a_pattern_scope_ignores_a_stray_phrase_index() {
    let (state, _dir) = state_from(PATTERN, "batch-pattern-phrase");
    let mut targets = moves(&[0], |_| 1.0);
    targets[0].phrase_index = 3;

    move_notes_at(&state, pattern(), &targets).expect("pattern scope ignores the phrase");

    assert_eq!(pattern_starts(&state), vec![0.25, 0.5, 0.75, 1.0]);
}

// ============================================================================
// Pattern bounds
// ============================================================================

fn add_pattern_note(state: &AppState, pattern_name: &str, start: f64) -> Result<(), String> {
    add_pattern_note_at(
        state,
        pattern_name,
        "G",
        "Natural",
        4,
        "q",
        start,
        Some(1.0),
        None,
    )
}

fn pattern_bars(state: &AppState) -> Option<f64> {
    state.lock_inner().project.patterns[0].bars
}

#[test]
fn a_note_past_the_last_bar_is_refused_with_the_boundary() {
    let (state, _dir) = state_from(PATTERN, "pattern-bounds-refuse");

    // The fixture's content ends at one quarter, so its last bar ends at four.
    let error = add_pattern_note(&state, "riff", 5.0).unwrap_err();

    assert!(error.contains("outside pattern 'riff'"), "{error}");
    assert!(error.contains("4 quarters"), "{error}");
    assert_eq!(pattern_starts(&state).len(), 4);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn a_note_inside_the_last_bar_is_still_accepted() {
    let (state, _dir) = state_from(PATTERN, "pattern-bounds-inside");

    add_pattern_note(&state, "riff", 2.0).expect("a note inside bar one is placed");

    assert_eq!(pattern_starts(&state), vec![0.0, 0.25, 0.5, 0.75, 2.0]);
    assert_eq!(undo_len(&state), 1);
}

#[test]
fn a_hit_past_the_last_bar_is_refused_with_the_boundary() {
    let (state, _dir) = state_from(PATTERN, "pattern-bounds-hit");

    let error =
        add_pattern_hit_at(&state, "riff", "kick", 5, 0, None, HitExtras::default()).unwrap_err();

    assert!(error.contains("4 quarters"), "{error}");
    assert!(state.lock_inner().project.patterns[0].hits.is_empty());
    add_pattern_hit_at(&state, "riff", "kick", 4, 3, None, HitExtras::default())
        .expect("the last sixteenth of bar one is inside");
    assert_eq!(state.lock_inner().project.patterns[0].hits.len(), 1);
    assert_eq!(undo_len(&state), 1);
}

#[test]
fn a_declared_length_is_the_placement_bound() {
    let (state, _dir) = state_from(LONG_PATTERN, "pattern-bounds-declared");
    set_pattern_bars_at(&state, "long", Some(0.25)).expect("declare one quarter");

    let error = add_pattern_note(&state, "long", 1.0).unwrap_err();

    assert!(error.contains("1 quarters"), "{error}");
    add_pattern_note(&state, "long", 0.5).expect("a note before the declared end is placed");
    assert_eq!(pattern_starts(&state), vec![0.0, 0.5, 4.0]);
}

#[test]
fn pattern_swing_distinguishes_explicit_zero_from_project_inheritance() {
    let (state, _dir) = state_from(LONG_PATTERN, "pattern-swing-inherit");
    let mut guard = state.lock_inner();
    let inner = &mut *guard;
    assert_eq!(inner.project.patterns[0].swing, None);

    set_pattern_swing_inner(inner, "long", Some(0.0)).unwrap();
    assert_eq!(inner.project.patterns[0].swing, Some(0.0));
    assert!(inner.dsl_source.contains("swing: 0%"));
    set_pattern_swing_inner(inner, "long", Some(101.0)).unwrap_err();
    assert_eq!(inner.project.patterns[0].swing, Some(0.0));
    assert_eq!(inner.undo.undo.len(), 1);

    set_pattern_swing_inner(inner, "long", None).unwrap();
    assert_eq!(inner.project.patterns[0].swing, None);
    assert!(!inner.dsl_source.contains("swing:"));
    assert_eq!(inner.undo.undo.len(), 2);
}

#[test]
fn project_swing_subdivision_switches_grid_and_is_undoable() {
    use oc_dsl::ast::SwingSubdivision;

    let (state, _dir) = state_from(LONG_PATTERN, "swing-subdivision");
    let mut guard = state.lock_inner();
    let inner = &mut *guard;
    assert_eq!(
        inner.project.properties.swing_subdivision,
        SwingSubdivision::Eighth
    );

    set_project_swing_subdivision_inner(inner, "16th").unwrap();
    assert_eq!(
        inner.project.properties.swing_subdivision,
        SwingSubdivision::Sixteenth
    );
    assert!(inner.dsl_source.contains("swing_subdivision: 16th"));
    assert_eq!(inner.undo.undo.len(), 1);

    // A value the DSL cannot write is refused without touching the document.
    set_project_swing_subdivision_inner(inner, "32nd").unwrap_err();
    assert_eq!(
        inner.project.properties.swing_subdivision,
        SwingSubdivision::Sixteenth
    );
    assert_eq!(inner.undo.undo.len(), 1);

    // Writing the value it already holds records nothing.
    set_project_swing_subdivision_inner(inner, "16th").unwrap();
    assert_eq!(inner.undo.undo.len(), 1);

    set_project_swing_subdivision_inner(inner, "8th").unwrap();
    assert_eq!(
        inner.project.properties.swing_subdivision,
        SwingSubdivision::Eighth
    );
    assert!(!inner.dsl_source.contains("swing_subdivision"));
    assert_eq!(inner.undo.undo.len(), 2);
}

#[test]
fn keeping_out_of_range_notes_records_one_undo_entry() {
    let (state, _dir) = state_from(LONG_PATTERN, "pattern-bounds-keep");

    set_pattern_bars_at(&state, "long", Some(1.0)).expect("shorten without trimming");

    assert_eq!(pattern_bars(&state), Some(1.0));
    assert_eq!(
        pattern_starts(&state),
        vec![0.0, 4.0],
        "the note in bar two stays in the document"
    );
    assert_eq!(undo_len(&state), 1);

    {
        let mut guard = state.lock_inner();
        let inner = &mut *guard;
        inner.undo.undo(&mut inner.project).expect("one undo entry");
    }
    assert_eq!(pattern_bars(&state), None);
    assert_eq!(pattern_starts(&state), vec![0.0, 4.0]);
}

#[test]
fn trimming_a_pattern_removes_notes_past_the_new_end_in_one_entry() {
    let (state, _dir) = state_from(LONG_PATTERN, "pattern-bounds-trim");

    trim_pattern_to_bars_at(&state, "long", 1.0).expect("trim to one bar");

    assert_eq!(pattern_bars(&state), Some(1.0));
    assert_eq!(pattern_starts(&state), vec![0.0]);
    assert_eq!(undo_len(&state), 1);

    {
        let mut guard = state.lock_inner();
        let inner = &mut *guard;
        inner.undo.undo(&mut inner.project).expect("one undo entry");
    }
    assert_eq!(pattern_bars(&state), None);
    assert_eq!(pattern_starts(&state), vec![0.0, 4.0]);
}

/// Three hits with distinct names, so a removal can only match one of them.
const HIT_PATTERN: &str = r#"@ocp "0.2.0"
@project "hit placement fixture"
  tempo: 120
  time_signature: 4/4
@pattern riff | instrument: drums/acoustic_kit
  hits:
    kick 1:0 mf
    snare 2:0 mf
    rim 3:0 mf
"#;

fn hit_names(state: &AppState) -> Vec<String> {
    state.lock_inner().project.patterns[0]
        .hits
        .iter()
        .map(|hit| hit.drum.clone())
        .collect()
}

/// One numeric modifier of the last hit, matched by name.
fn last_hit_modifier(state: &AppState, key: &str) -> Option<f64> {
    let inner = state.lock_inner();
    inner.project.patterns[0]
        .hits
        .last()?
        .modifiers
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| match value {
            oc_dsl::ast::ParamValue::Number(number) => Some(*number),
            _ => None,
        })
}

#[test]
fn a_hit_writes_the_microtiming_and_note_the_grid_needs() {
    let (state, _dir) = state_from(HIT_PATTERN, "hit-extras");

    let extras = HitExtras {
        offset_beats: Some(1.0 / 12.0),
        midi_note: Some(50),
        ..HitExtras::default()
    };
    add_pattern_hit_at(&state, "riff", "pad_50", 1, 1, None, extras)
        .expect("a triplet hit inside the first beat is placed");

    let position = {
        let inner = state.lock_inner();
        inner.project.patterns[0]
            .hits
            .last()
            .expect("the new hit")
            .position
    };
    assert_eq!(
        position,
        oc_dsl::ast::BeatPosition {
            beat: 1,
            subdivision: 1
        }
    );
    let offset = last_hit_modifier(&state, "timing_offset_beats").expect("the microtiming");
    assert!((offset - 1.0 / 12.0).abs() < 1e-6, "{offset}");
    assert_eq!(last_hit_modifier(&state, "midi_note"), Some(50.0));

    // The written document keeps both values, so reopening the project sounds
    // the same hit on the same note. It also has to parse and validate, or the
    // edit would be lost the next time the project is loaded.
    let dsl = state.lock_inner().dsl_source.clone();
    assert!(dsl.contains("pad_50 1:1"), "{dsl}");
    assert!(dsl.contains("midi_note=50"), "{dsl}");
    assert!(dsl.contains("timing_offset_beats=0.0833"), "{dsl}");
    let reparsed = oc_dsl::parse(&dsl).expect("the written document parses");
    oc_dsl::validate(&reparsed).expect("the written document validates");
    let hit = reparsed.patterns[0].hits.last().expect("the new hit");
    let written = f64::from(hit.position.beat - 1) + f64::from(hit.position.subdivision) * 0.25;
    let microtiming = hit
        .modifiers
        .iter()
        .find_map(|(name, value)| match (name.as_str(), value) {
            ("timing_offset_beats", oc_dsl::ast::ParamValue::Number(number)) => Some(*number),
            _ => None,
        })
        .expect("the microtiming survived the round trip");
    assert!((written + microtiming - (0.25 + 1.0 / 12.0)).abs() < 1e-3);
}

#[test]
fn a_grid_hit_before_the_pattern_end_round_trips_its_exact_onset() {
    let (state, _dir) = state_from(HIT_PATTERN, "hit-grid-edge");
    let onset = 23.0 / 6.0;
    add_pattern_hit_at(
        &state,
        "riff",
        "kick",
        4,
        3,
        None,
        HitExtras {
            grid_offset_beats: Some(onset - 3.75),
            ..HitExtras::default()
        },
    )
    .unwrap();
    let source = state.lock_inner().dsl_source.clone();
    let reopened = oc_dsl::parse(&source).unwrap();
    oc_dsl::validate(&reopened).unwrap();
    let hit = reopened.patterns[0].hits.last().unwrap();
    assert_eq!(hit.written_quarters(), onset);
    assert_eq!(undo_len(&state), 1);
}

#[test]
fn a_hit_microtiming_past_the_limit_is_refused_without_a_write() {
    let (state, _dir) = state_from(HIT_PATTERN, "hit-extras-limit");

    let extras = HitExtras {
        offset_beats: Some(0.25),
        midi_note: None,
        ..HitExtras::default()
    };
    let error = add_pattern_hit_at(&state, "riff", "kick", 2, 0, None, extras).unwrap_err();

    assert!(error.contains("outside"), "{error}");
    assert_eq!(hit_names(&state), vec!["kick", "snare", "rim"]);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn a_hit_note_outside_the_midi_range_is_refused() {
    let (state, _dir) = state_from(HIT_PATTERN, "hit-extras-note");

    let extras = HitExtras {
        offset_beats: None,
        midi_note: Some(200),
        ..HitExtras::default()
    };
    let error = add_pattern_hit_at(&state, "riff", "pad_200", 2, 0, None, extras).unwrap_err();

    assert!(error.contains("MIDI range"), "{error}");
    assert_eq!(hit_names(&state), vec!["kick", "snare", "rim"]);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn a_hit_name_that_is_not_an_identifier_is_refused() {
    let (state, _dir) = state_from(HIT_PATTERN, "hit-extras-name");

    let error = add_pattern_hit_at(
        &state,
        "riff",
        "kick drum",
        2,
        0,
        None,
        HitExtras::default(),
    )
    .unwrap_err();

    assert!(error.contains("not a drum name"), "{error}");
    assert_eq!(hit_names(&state), vec!["kick", "snare", "rim"]);
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn a_hit_before_the_first_beat_is_refused() {
    let (state, _dir) = state_from(HIT_PATTERN, "hit-extras-beat-zero");

    let error =
        add_pattern_hit_at(&state, "riff", "kick", 0, 0, None, HitExtras::default()).unwrap_err();

    assert!(error.contains("beat 1"), "{error}");
    assert_eq!(undo_len(&state), 0);
}

#[test]
fn removing_a_hit_acts_on_the_index_the_pattern_holds() {
    let (state, _dir) = state_from(HIT_PATTERN, "hit-remove-index");

    // A view can drop the hits before it, but the index it reports is still the
    // pattern's: removing 2 takes `rim` and leaves the hits before it.
    remove_pattern_hit_at(&state, "riff", 2).expect("the third hit is removed");

    assert_eq!(hit_names(&state), vec!["kick", "snare"]);
    assert_eq!(undo_len(&state), 1);
}

#[test]
fn removing_a_hit_past_the_end_is_refused_without_a_write() {
    let (state, _dir) = state_from(HIT_PATTERN, "hit-remove-range");

    let error = remove_pattern_hit_at(&state, "riff", 3).unwrap_err();

    assert!(error.contains("out of range"), "{error}");
    assert_eq!(hit_names(&state), vec!["kick", "snare", "rim"]);
    assert_eq!(undo_len(&state), 0);
}
