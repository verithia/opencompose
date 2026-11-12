//! Renaming a pattern edits the AST: the declaration and every clip that
//! references it move together, in one undo entry. An audio clip that happens
//! to share the name keeps it.

use crate::commands::notes::rename_pattern_blocking;

use super::support::*;

const SOURCE: &str = r#"@ocp "0.2.0"
@project "pattern rename"
  tempo: 120
  time_signature: 4/4

@pattern "My Riff" | instrument: synth/lead_square
  events:
    C4 q mf

@pattern "Beat" | instrument: drums/acoustic_kit
  hits:
    kick 1:0 ff

@pattern "Beat 2" | instrument: drums/acoustic_kit
  hits:
    snare 1:0 ff

@audio_clip "My Riff" [9:1 - 10:1] | track: vocal
  file: "riff.wav"

@vocal_tune tune | clip: "My Riff"
  amount: 100%

@track lead | instrument: synth/lead_square
  clip: "My Riff" [1:1 - 2:1]

@track drums | instrument: drums/acoustic_kit
  clip: "Beat" [1:1 - 2:1]
  clip: "Beat 2" [3:1 - 4:1]
  clip: "My Riff" [5:1 - 6:1]

@track vocal | instrument: audio/clip
"#;

fn state() -> (crate::state::AppState, TempDir) {
    let dir = TempDir::new("pattern-rename");
    let state = state_with_project(SOURCE, dir.path());
    (state, dir)
}

fn clip_names(project: &oc_dsl::ast::Project, track: usize) -> Vec<String> {
    project.tracks[track]
        .pattern_clips()
        .map(|clip| clip.pattern.clone())
        .collect()
}

#[test]
fn a_quoted_name_renames_the_pattern_and_every_clip() {
    let (state, _dir) = state();

    let snapshot = rename_pattern_blocking(&state, "My Riff", "Verse 2").expect("the rename lands");

    assert_eq!(snapshot.project.patterns[0].name, "Verse 2");
    assert_eq!(clip_names(&snapshot.project, 0), ["Verse 2"]);
    assert_eq!(
        clip_names(&snapshot.project, 1),
        ["Beat", "Beat 2", "Verse 2"]
    );
    assert!(
        snapshot.dsl_source.contains(r#"@pattern "Verse 2""#),
        "the saved document carries the new declaration:\n{}",
        snapshot.dsl_source
    );
    assert!(
        snapshot.dsl_source.contains(r#"clip: "Verse 2""#),
        "the saved document carries the new reference:\n{}",
        snapshot.dsl_source
    );
}

#[test]
fn a_unicode_name_renames_verbatim() {
    let (state, _dir) = state();

    let snapshot = rename_pattern_blocking(&state, "My Riff", "主歌 2").expect("the rename lands");

    assert_eq!(snapshot.project.patterns[0].name, "主歌 2");
    assert_eq!(clip_names(&snapshot.project, 0), ["主歌 2"]);
    assert!(snapshot.dsl_source.contains(r#"@pattern "主歌 2""#));
}

#[test]
fn an_audio_clip_that_shares_the_name_keeps_it() {
    let (state, _dir) = state();

    let snapshot = rename_pattern_blocking(&state, "My Riff", "Verse 2").expect("the rename lands");

    let audio = snapshot
        .project
        .audio_clips()
        .into_iter()
        .find(|clip| clip.name.contains("Riff"))
        .expect("the audio clip survives");
    assert_eq!(audio.name, "My Riff");
    let tune = &snapshot.project.properties.vocal_tunes[0];
    assert_eq!(tune.clip.as_deref(), Some("My Riff"));
}

#[test]
fn one_undo_restores_the_pattern_and_its_clips() {
    let (state, _dir) = state();
    rename_pattern_blocking(&state, "My Riff", "Verse 2").expect("the rename lands");

    let mut inner = state.lock_inner();
    assert_eq!(inner.undo.undo.len(), 1, "one edit is one undo entry");
    let mut project = inner.project.clone();
    inner.undo.undo(&mut project).expect("one undo entry");

    // The patch layer keys patterns by name, so undo restores the name without
    // promising the old position; the references are what must come back.
    let names: Vec<&str> = project.patterns.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names.len(), 3);
    assert!(
        names.contains(&"My Riff"),
        "undo restores the name: {names:?}"
    );
    assert!(
        !names.contains(&"Verse 2"),
        "the new name is gone: {names:?}"
    );
    assert_eq!(clip_names(&project, 0), ["My Riff"]);
    assert_eq!(clip_names(&project, 1), ["Beat", "Beat 2", "My Riff"]);
}

#[test]
fn a_duplicate_name_is_refused_and_changes_nothing() {
    let (state, _dir) = state();

    let error = rename_pattern_blocking(&state, "My Riff", "Beat 2")
        .expect_err("a duplicate name must be refused");
    assert!(error.contains("Beat 2"), "{error}");

    let inner = state.lock_inner();
    assert!(
        !inner.undo.can_undo(),
        "a refused edit pushes no undo entry"
    );
    assert_eq!(inner.project.patterns[0].name, "My Riff");
    assert_eq!(clip_names(&inner.project, 0), ["My Riff"]);
}

#[test]
fn an_empty_name_is_refused_and_changes_nothing() {
    let (state, _dir) = state();

    let error = rename_pattern_blocking(&state, "My Riff", "   ")
        .expect_err("an empty name must be refused");
    assert!(error.to_lowercase().contains("empty"), "{error}");

    let inner = state.lock_inner();
    assert!(
        !inner.undo.can_undo(),
        "a refused edit pushes no undo entry"
    );
    assert_eq!(inner.project.patterns[0].name, "My Riff");
}

#[test]
fn a_name_that_prefixes_another_pattern_leaves_it_untouched() {
    let (state, _dir) = state();

    let snapshot =
        rename_pattern_blocking(&state, "Beat", "Verse").expect("the prefix rename lands");

    assert_eq!(snapshot.project.patterns[1].name, "Verse");
    assert_eq!(
        snapshot.project.patterns[2].name, "Beat 2",
        "the longer name keeps its own text"
    );
    assert_eq!(clip_names(&snapshot.project, 0), ["My Riff"]);
    assert_eq!(
        clip_names(&snapshot.project, 1),
        ["Verse", "Beat 2", "My Riff"]
    );
}
