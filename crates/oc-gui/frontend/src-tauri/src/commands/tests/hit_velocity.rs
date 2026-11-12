//! A drum hit's exact velocity: one drag writes one value, one undo entry, and
//! clearing it puts the written dynamic back in charge.

use oc_dsl::ast::Dynamics;

use crate::commands::notes::{clear_pattern_hit_velocity_at, set_pattern_hit_velocity_at};
use crate::state::AppState;

use super::support::{TempDir, state_with_project};

/// Two hits: one with an exact velocity, one that follows its dynamic token.
const HITS: &str = r#"@ocp "0.2.0"
@project "hit velocity fixture"
  tempo: 120
  time_signature: 4/4
@pattern riff | instrument: drums/acoustic_kit
  hits:
    kick 1:0 mf v=88
    snare 2:0 mp
"#;

fn state() -> (AppState, TempDir) {
    let dir = TempDir::new("hit-velocity");
    let state = state_with_project(HITS, dir.path());
    (state, dir)
}

fn undo_len(state: &AppState) -> usize {
    state.lock_inner().undo.undo.len()
}

#[test]
fn a_drag_writes_the_exact_velocity_in_one_undo_entry() {
    let (state, _dir) = state();
    let before = undo_len(&state);

    let snapshot =
        set_pattern_hit_velocity_at(&state, "riff", 0, 111).expect("the velocity writes");

    assert_eq!(snapshot.project.patterns[0].hits[0].velocity, Some(111));
    assert_eq!(undo_len(&state), before + 1);
    assert!(
        snapshot.dsl_source.contains("kick 1:0 mf v=111"),
        "{}",
        snapshot.dsl_source
    );
}

#[test]
fn writing_the_same_value_records_nothing() {
    let (state, _dir) = state();
    let before = undo_len(&state);

    set_pattern_hit_velocity_at(&state, "riff", 0, 88).expect("the value is unchanged");
    assert_eq!(undo_len(&state), before);
}

#[test]
fn a_velocity_outside_the_range_is_refused() {
    let (state, _dir) = state();
    let before = undo_len(&state);

    for bad in [0u8, 128, 255] {
        let error = set_pattern_hit_velocity_at(&state, "riff", 0, bad).expect_err("out of range");
        assert!(error.contains("1 and 127"), "{error}");
    }
    assert_eq!(
        state.lock_inner().project.patterns[0].hits[0].velocity,
        Some(88)
    );
    assert_eq!(undo_len(&state), before);
}

#[test]
fn an_unknown_pattern_or_hit_is_named_in_the_refusal() {
    let (state, _dir) = state();

    let error = set_pattern_hit_velocity_at(&state, "nope", 0, 100).expect_err("no such pattern");
    assert!(error.contains("Pattern 'nope' not found"), "{error}");

    let error = set_pattern_hit_velocity_at(&state, "riff", 9, 100).expect_err("no such hit");
    assert!(error.contains("Hit 9 out of range"), "{error}");
}

#[test]
fn clearing_leaves_the_written_dynamic_in_charge() {
    let (state, _dir) = state();
    let snapshot = clear_pattern_hit_velocity_at(&state, "riff", 0).expect("the velocity clears");

    let hit = &snapshot.project.patterns[0].hits[0];
    assert_eq!(hit.velocity, None);
    assert_eq!(hit.dynamics, Some(Dynamics::MF));
    assert!(
        snapshot.dsl_source.contains("kick 1:0 mf"),
        "{}",
        snapshot.dsl_source
    );
    assert!(
        !snapshot.dsl_source.contains("v=88"),
        "{}",
        snapshot.dsl_source
    );
}

#[test]
fn clearing_a_hit_that_writes_no_value_records_nothing() {
    let (state, _dir) = state();
    let before = undo_len(&state);

    // The snare writes `mp` only, so there is nothing to clear.
    clear_pattern_hit_velocity_at(&state, "riff", 1).expect("clearing is a no-op");
    assert_eq!(undo_len(&state), before);
}
