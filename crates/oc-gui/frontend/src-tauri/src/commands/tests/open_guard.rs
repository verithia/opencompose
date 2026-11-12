//! Opening another project must not drop an agent batch's pre-commit edits.
//!
//! The batch's edits live only in memory until it commits, so a replacement
//! refuses while one is open and answers the same sentence for Open and New.
//! The document, its dirty flag, the revision and the batch itself stay as
//! they were, so the agent can commit or roll back against the same document.

use crate::commands::replace_project;
use crate::state::{AppState, TRANSACTION_BLOCKS_REPLACEMENT};

use super::support::{TempDir, make_track, state_with_project};

const SONG: &str = r#"@ocp "0.2.0"

@project "open guard song"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf

@track keys | instrument: piano/acoustic_grand
  effects:
    - gain: factor=0.5
  clip: riff [1:1 - 3:1]
"#;

fn state_in_batch(label: &str) -> (TempDir, AppState) {
    let dir = TempDir::new(label);
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = state_with_project(SONG, dir.path());
    {
        let mut inner = state.lock_inner();
        inner.dirty = true;
        inner
            .begin_transaction("agent batch".into(), "agent".into())
            .unwrap();
    }
    (dir, state)
}

#[test]
fn replacement_is_refused_while_a_batch_is_open() {
    let (_dir, state) = state_in_batch("open-guard-batch");
    let mut inner = state.lock_inner();
    let before_project = oc_dsl::serialize(&inner.project);
    let before_dirty = inner.dirty;
    let before_revision = inner.revision;
    let before_label = inner.transaction.as_ref().unwrap().label.clone();

    let mut replacement = inner.project.clone();
    replacement.name = Some("Another song".into());
    replacement.tracks.push(make_track("pad"));

    let error = replace_project(&mut inner, replacement, None).unwrap_err();

    assert_eq!(error, TRANSACTION_BLOCKS_REPLACEMENT);
    assert_eq!(oc_dsl::serialize(&inner.project), before_project);
    assert_eq!(inner.dirty, before_dirty);
    assert_eq!(inner.revision, before_revision);
    assert_eq!(inner.transaction.as_ref().unwrap().label, before_label);
}

#[test]
fn replacement_is_allowed_once_the_batch_is_closed() {
    let (_dir, state) = state_in_batch("open-guard-commit");
    let mut inner = state.lock_inner();
    inner.commit_transaction().unwrap();

    let mut replacement = inner.project.clone();
    replacement.name = Some("Another song".into());

    let snapshot = replace_project(&mut inner, replacement, None).unwrap();

    assert_eq!(snapshot.project.name.as_deref(), Some("Another song"));
    assert!(inner.transaction.is_none());
}
