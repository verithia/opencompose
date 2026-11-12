//! Edit-time media validation: a document must not name media that is not there.

use crate::commands::*;

use super::support::{TempDir, state_with_project};

#[test]
fn a_preset_file_that_does_not_exist_is_rejected_at_validate_and_apply() {
    let dir = TempDir::new("preset-missing");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = state_with_project("@project\n  tempo: 120\n", dir.path());
    let dsl = "@preset grand\n  file: \"media/grand.sf2\"\n";

    let inner = state.lock_inner();
    let check = validate_edit_in_project(&inner, dsl.into(), "preset:grand".into());
    drop(inner);
    assert!(!check.valid);
    assert!(
        check
            .errors
            .iter()
            .any(|error| error.contains("media/grand.sf2")),
        "{:?}",
        check.errors
    );
    assert!(
        check
            .errors
            .iter()
            .any(|error| error.contains(&dir.path().display().to_string())),
        "the error must name the base directory: {:?}",
        check.errors
    );
    assert!(
        check.errors.iter().any(|error| {
            error
                .split_once(':')
                .is_some_and(|(line, _)| line.parse::<u32>().is_ok())
        }),
        "the media error should carry a one-based line: {:?}",
        check.errors
    );

    let mut inner = state.lock_inner();
    let expected = inner.dsl_source.clone();
    let error =
        apply_edit_to_project(&mut inner, dsl.into(), "preset:grand".into(), expected).unwrap_err();
    assert!(error.contains("media/grand.sf2"), "{error}");
    assert!(
        inner.project.presets.is_empty(),
        "a failed edit must not apply"
    );
    drop(inner);

    std::fs::create_dir_all(dir.path().join("media")).unwrap();
    std::fs::write(dir.path().join("media/grand.sf2"), b"soundfont").unwrap();
    let mut inner = state.lock_inner();
    let expected = inner.dsl_source.clone();
    let snapshot =
        apply_edit_to_project(&mut inner, dsl.into(), "preset:grand".into(), expected).unwrap();
    assert_eq!(snapshot.project.presets.len(), 1);
    assert_eq!(snapshot.project.presets[0].name, "grand");
}

#[test]
fn builtin_presets_need_no_soundfont_file() {
    let dir = TempDir::new("preset-builtin");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = state_with_project("@project\n  tempo: 120\n", dir.path());
    let inner = state.lock_inner();
    let check = validate_edit_in_project(
        &inner,
        "@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n".into(),
        "preset:tone".into(),
    );
    assert!(check.valid, "{:?}", check.errors);
}
