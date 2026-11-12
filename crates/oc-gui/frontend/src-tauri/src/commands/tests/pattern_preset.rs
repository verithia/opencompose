//! The pattern-preset command: one undoable edit that moves the pattern's
//! preset and leaves every hosting track's preset where it was.

use crate::commands::synth::set_pattern_preset_blocking;

use super::support::*;

const SOURCE: &str = r#"@ocp "0.2.0"
@project "pattern preset"
  tempo: 120
  time_signature: 4/4

@preset bright
  source: builtin
  plugin: "builtin.open_synth"

@preset dark
  source: builtin
  plugin: "builtin.open_synth"

@pattern riff | preset: bright, instrument: synth/lead_square
  events:
    E4 q mf

@track lead | preset: bright, instrument: synth/lead_square
  clip: riff [1:1 - 2:1]

@track keys | preset: dark, instrument: synth/lead_square
  @phrase line [1:1 - 2:1]
    C4 q mf
"#;

fn state() -> (crate::state::AppState, TempDir) {
    let dir = TempDir::new("pattern-preset");
    let state = state_with_project(SOURCE, dir.path());
    (state, dir)
}

#[test]
fn setting_a_pattern_preset_moves_only_the_pattern() {
    let (state, _dir) = state();

    let snapshot = set_pattern_preset_blocking(&state, "riff".into(), Some("dark".into()))
        .expect("the pattern preset lands");

    assert_eq!(snapshot.project.patterns[0].preset.as_deref(), Some("dark"));
    // The two tracks keep their own presets; the pattern overrides them.
    assert_eq!(snapshot.project.tracks[0].preset.as_deref(), Some("bright"));
    assert_eq!(snapshot.project.tracks[1].preset.as_deref(), Some("dark"));
    assert!(
        snapshot.dsl_source.contains("preset: dark"),
        "the saved document carries the new pattern preset"
    );
}

#[test]
fn clearing_a_pattern_preset_releases_the_override() {
    let (state, _dir) = state();
    set_pattern_preset_blocking(&state, "riff".into(), Some("dark".into())).unwrap();

    let snapshot = set_pattern_preset_blocking(&state, "riff".into(), None)
        .expect("clearing the pattern preset lands");

    // The resync may inject the family default the editor displays; that is
    // not an authored reference, so it does not override the track.
    let preset = snapshot.project.patterns[0].preset.as_deref();
    assert_ne!(preset, Some("dark"));
    assert!(
        preset.is_none_or(oc_dsl::instruments::is_injected),
        "unexpected pattern preset after clearing: {preset:?}"
    );
    assert_eq!(snapshot.project.tracks[0].preset.as_deref(), Some("bright"));
}

#[test]
fn one_pattern_preset_edit_is_one_undo_entry() {
    let (state, _dir) = state();

    set_pattern_preset_blocking(&state, "riff".into(), Some("dark".into())).unwrap();

    let mut inner = state.lock_inner();
    assert_eq!(inner.undo.undo.len(), 1, "one edit is one undo entry");
    let mut project = inner.project.clone();
    inner.undo.undo(&mut project).expect("one undo entry");
    assert_eq!(
        project.patterns[0].preset.as_deref(),
        Some("bright"),
        "undo restores the pattern's previous preset"
    );
    assert_eq!(
        project.tracks[0].preset.as_deref(),
        Some("bright"),
        "undo leaves the hosting tracks alone"
    );
}

#[test]
fn an_unknown_pattern_is_refused_without_an_edit() {
    let (state, _dir) = state();

    let error = set_pattern_preset_blocking(&state, "missing".into(), Some("dark".into()))
        .expect_err("an unknown pattern must be refused");
    assert!(error.contains("missing"), "{error}");

    let inner = state.lock_inner();
    assert!(
        !inner.undo.can_undo(),
        "a refused edit pushes no undo entry"
    );
    assert_eq!(inner.project.patterns[0].preset.as_deref(), Some("bright"));
}
