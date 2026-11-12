//! Opening a project keeps the file's text: comments and hand formatting
//! survive a save, and a document the loader had to change is regenerated.

use std::path::PathBuf;

use super::support::TempDir;

use crate::commands::project::{open_project, save_file_inner, update_dsl_inner};

/// A project whose text parses to exactly the document the loader installs.
const HAND_WRITTEN: &str = r#"// hand-written: this comment must survive a save
@ocp   "0.2.0"

@project    "text fidelity"
      tempo:  120
      time_signature: 4/4

@preset hand_keys
      source: soundfont
      program: 0

@track keys | preset: hand_keys, instrument: piano/acoustic_grand
"#;

/// A bare instrument: the loader injects its family default, so the document
/// it installs no longer matches the text.
const BARE_INSTRUMENT: &str = r#"@ocp "0.2.0"

@project "bare"
  tempo: 120

@track lead | instrument: piano/acoustic_grand
"#;

/// A file written before the version header: the loader adds one, so the
/// installed document no longer matches the text.
const VERSIONLESS: &str = r#"// written before the version header
@project "old"
  tempo: 120
"#;

/// Put `source` on disk and open it exactly as the open command does.
fn open_from(dir: &TempDir, name: &str, source: &str) -> (crate::state::AppState, PathBuf) {
    std::fs::create_dir_all(dir.path()).unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, source).unwrap();
    let state = crate::state::AppState::new();
    open_project(&mut state.lock_inner(), source, Some(path.clone())).unwrap();
    (state, path)
}

#[test]
fn a_hand_written_project_round_trips_byte_for_byte() {
    let dir = TempDir::new("text-fidelity-round-trip");
    let (state, path) = open_from(&dir, "hand.ocp", HAND_WRITTEN);

    // The editor reads the same text the file holds.
    assert_eq!(state.lock_inner().dsl_source, HAND_WRITTEN);

    save_file_inner(None, &state).unwrap();

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        HAND_WRITTEN,
        "a save must not reformat the file or drop its comment"
    );
    assert_eq!(state.lock_inner().dsl_source, HAND_WRITTEN);
}

#[test]
fn an_injected_default_regenerates_the_source() {
    let dir = TempDir::new("text-fidelity-default");
    let (state, path) = open_from(&dir, "bare.ocp", BARE_INSTRUMENT);

    let inner = state.lock_inner();
    assert_ne!(
        inner.dsl_source, BARE_INSTRUMENT,
        "the injected preset must be visible in the DSL"
    );
    assert!(inner.dsl_source.contains("@preset oc_default_keys"));
    assert_eq!(
        oc_dsl::parse(&inner.dsl_source).unwrap(),
        inner.project,
        "the regenerated text describes the installed document"
    );
    drop(inner);

    save_file_inner(None, &state).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    let inner = state.lock_inner();
    assert_eq!(written, inner.dsl_source);
    assert_eq!(oc_dsl::parse(&written).unwrap(), inner.project);
}

#[test]
fn an_added_version_regenerates_the_source() {
    let dir = TempDir::new("text-fidelity-version");
    let (state, path) = open_from(&dir, "old.ocp", VERSIONLESS);

    let inner = state.lock_inner();
    assert!(inner.dsl_source.starts_with("@ocp \"0.6.0\""));
    assert_eq!(oc_dsl::parse(&inner.dsl_source).unwrap(), inner.project);
    drop(inner);

    save_file_inner(None, &state).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written, state.lock_inner().dsl_source);
    assert!(
        !written.contains("written before the version header"),
        "the upgrade rewrites the text, as it did before"
    );
}

#[test]
fn update_dsl_keeps_the_text_it_was_given() {
    let dir = TempDir::new("text-fidelity-update-dsl");
    let (state, path) = open_from(&dir, "hand.ocp", HAND_WRITTEN);
    let edited = format!("{HAND_WRITTEN}\n// an edit made in the editor\n");

    let (snapshot, _root) = {
        let mut inner = state.lock_inner();
        let result = update_dsl_inner(edited.clone(), &mut inner).unwrap();
        assert_eq!(inner.dsl_source, edited);
        result
    };
    assert_eq!(snapshot.dsl_source, edited);

    save_file_inner(None, &state).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
}
