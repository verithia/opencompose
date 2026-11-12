//! File ▸ New answers before the graph warm-up, and a newer document wins.
//!
//! The command installs the document and warms behind the response, like Open;
//! the warm re-checks the revision under the same lock it holds, so a second
//! New, or an edit, is never overwritten by work queued for an older document.

use std::time::Instant;

use crate::commands::{
    create_project_in, new_project_document, replace_project, warm_preload_if_current,
};
use crate::state::AppState;

use super::support::TempDir;

fn install(state: &AppState, path: Option<std::path::PathBuf>) -> crate::commands::ProjectSnapshot {
    let mut inner = state.lock_inner();
    create_project_in(&mut inner, path).unwrap()
}

#[test]
fn new_project_answers_before_the_warm_up_opens_the_device() {
    let state = AppState::new();
    let snapshot = install(&state, None);
    // The response is out before any device work: no worker, no stream, no build.
    {
        let inner = state.lock_inner();
        assert!(
            inner.audio.render_thread.is_none(),
            "New opened the device inline"
        );
        assert!(
            !inner.audio.streaming_active,
            "New started the stream inline"
        );
    }
    assert!(warm_preload_if_current(&state, snapshot.revision));
    let inner = state.lock_inner();
    assert!(
        inner.audio.render_thread.is_some(),
        "the warm-up never built the graph"
    );
    assert!(
        inner.audio.streaming_active,
        "the warm-up never opened the device"
    );
}

/// A second New lands while the first document's warm-up is still queued. The
/// warm-up must leave the newer document alone instead of building the old one
/// over it.
#[test]
fn a_second_new_makes_the_earlier_warm_up_a_no_op() {
    let state = AppState::new();
    let first = install(&state, None);
    let second = install(&state, None);
    assert_ne!(first.revision, second.revision);

    assert!(!warm_preload_if_current(&state, first.revision));
    assert!(
        state.lock_inner().audio.render_thread.is_none(),
        "the stale warm-up built the graph anyway"
    );

    assert!(warm_preload_if_current(&state, second.revision));
    let inner = state.lock_inner();
    assert_eq!(inner.revision, second.revision);
    assert!(inner.audio.render_thread.is_some());
}

/// An edit after the response is the same case: the warm-up was armed with a
/// revision the document no longer has.
#[test]
fn an_edit_after_new_makes_the_warm_up_a_no_op() {
    let state = AppState::new();
    let snapshot = install(&state, None);
    let edited = {
        let mut inner = state.lock_inner();
        let old = inner.project.clone();
        inner.project.name = Some("Edited".into());
        crate::commands::resync_with_undo(
            &mut inner,
            old,
            crate::state::UndoEdit::new("project name", crate::state::UndoScope::Project),
        )
        .unwrap();
        inner.revision
    };
    assert_ne!(edited, snapshot.revision);

    assert!(!warm_preload_if_current(&state, snapshot.revision));
    let inner = state.lock_inner();
    assert_eq!(
        inner.revision, edited,
        "the stale warm-up disturbed the edited document"
    );
    assert_eq!(inner.project.name.as_deref(), Some("Edited"));
}

/// The workspace's New is one command: the name comes from the file, and the
/// file is written before the snapshot leaves, so no rename or second save is
/// needed to make the directory list the project.
#[test]
fn a_new_project_at_a_path_is_named_and_written_before_the_listing() {
    let dir = TempDir::new("new-project-at");
    std::fs::create_dir_all(dir.path()).unwrap();
    let target = dir.path().join("Quiet Morning.ocp");

    let state = AppState::new();
    let snapshot = install(&state, Some(target.clone()));

    assert_eq!(snapshot.project.name.as_deref(), Some("Quiet Morning"));
    assert_eq!(
        snapshot.file_path.as_deref(),
        Some(target.to_str().unwrap())
    );
    assert!(!snapshot.dirty, "a freshly written project is not dirty");
    let written = std::fs::read_to_string(&target).unwrap();
    assert!(written.starts_with("@ocp \"0.6.0\""));
    assert!(
        written.contains("Quiet Morning"),
        "the file was written with another name:\n{written}"
    );
    // The document is on disk but the graph is still cold: the warm-up owns it.
    assert!(state.lock_inner().audio.render_thread.is_none());
}

/// A directory that refuses the file leaves an untitled document, not one that
/// claims a path nothing was written to.
#[test]
fn a_new_project_at_a_missing_directory_claims_no_path() {
    let dir = TempDir::new("new-project-missing");
    let target = dir.path().join("missing").join("Untitled.ocp");

    let state = AppState::new();
    let mut inner = state.lock_inner();
    assert!(create_project_in(&mut inner, Some(target)).is_err());
    assert!(
        inner.file_path.is_none(),
        "the document kept an unwritten path"
    );
    assert!(inner.audio.resource_root.is_none());
    assert_eq!(inner.project.name.as_deref(), Some("Untitled"));
    assert!(inner.audio.render_thread.is_none());
}

/// New keeps the replacement guard: an agent batch's pre-commit edits are not
/// dropped by a document replacement.
#[test]
fn new_project_refuses_while_an_agent_batch_is_open() {
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        inner
            .begin_transaction("agent batch".into(), "agent".into())
            .unwrap();
        let error = create_project_in(&mut inner, None).unwrap_err();
        assert_eq!(error, crate::state::TRANSACTION_BLOCKS_REPLACEMENT);
        assert!(inner.transaction.is_some());
    }
    assert!(state.lock_inner().audio.render_thread.is_none());
}

/// The New response time, headlessly. The sink is the simulated device: it
/// prices the dispatch, not the hardware open, so the warm figures are a lower
/// bound on a real device.
#[test]
#[ignore = "benchmark harness; run with --ignored --nocapture"]
fn measure_new_project_response_time() {
    for _ in 0..8 {
        // File ▸ New: the response installs the document and returns.
        let state = AppState::new();
        let started = Instant::now();
        let snapshot = install(&state, None);
        let answered = started.elapsed();
        let warm_started = Instant::now();
        let warmed = warm_preload_if_current(&state, snapshot.revision);
        let warm = warm_started.elapsed();

        // The workspace route: the same plus the file write.
        let state = AppState::new();
        let dir = TempDir::new("new-project-timing");
        std::fs::create_dir_all(dir.path()).unwrap();
        let target = dir.path().join("Untitled.ocp");
        let started = Instant::now();
        let snapshot = install(&state, Some(target));
        let answered_at = started.elapsed();
        let warm_started = Instant::now();
        let warmed_at = warm_preload_if_current(&state, snapshot.revision);
        let warm_at = warm_started.elapsed();

        // Baseline comparison: synchronous inline replacement under the state lock.
        let state = AppState::new();
        let started = Instant::now();
        {
            let mut inner = state.lock_inner();
            replace_project(&mut inner, new_project_document("Untitled".into()), None).unwrap();
        }
        let inline = started.elapsed();

        eprintln!(
            "[measure] new_project answered={answered:?} warm={warm:?} warmed={warmed} \
             answered_at_path={answered_at:?} warm_at_path={warm_at:?} warmed_at_path={warmed_at} \
             inline_before={inline:?}"
        );
    }
}

#[test]
fn starter_is_written_complete_with_builtins_before_installation() {
    let dir = TempDir::new("starter-complete");
    std::fs::create_dir_all(dir.path()).unwrap();
    let path = dir.path().join("My sketch.ocp");
    let state = AppState::new();
    let mut inner = state.lock_inner();
    let snap = crate::commands::create_starter_project_in(&mut inner, path.clone()).unwrap();
    assert_eq!(snap.project.name.as_deref(), Some("My sketch"));
    assert_eq!(snap.project.tracks.len(), 3);
    assert!(
        snap.project
            .tracks
            .iter()
            .all(|track| !track.phrases.is_empty())
    );
    assert!(
        snap.project
            .presets
            .iter()
            .all(|preset| preset.plugin.as_deref() == Some("builtin.open_synth"))
    );
    let saved = oc_dsl::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(saved, snap.project);
    assert!(!snap.dirty);
    assert!(inner.audio.render_thread.is_none());
}

#[test]
fn starter_never_overwrites_an_existing_file_or_document() {
    let dir = TempDir::new("starter-exists");
    std::fs::create_dir_all(dir.path()).unwrap();
    let path = dir.path().join("Existing.ocp");
    std::fs::write(&path, "existing work").unwrap();
    let state = AppState::new();
    let mut inner = state.lock_inner();
    let before = inner.project.clone();
    let revision = inner.revision;
    assert!(crate::commands::create_starter_project_in(&mut inner, path.clone()).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "existing work");
    assert_eq!(inner.project, before);
    assert_eq!(inner.revision, revision);
}

#[test]
fn starter_write_failure_keeps_the_current_document() {
    let dir = TempDir::new("starter-write-failure");
    let path = dir.path().join("missing").join("Sketch.ocp");
    let state = AppState::new();
    let mut inner = state.lock_inner();
    let before = inner.project.clone();
    assert!(crate::commands::create_starter_project_in(&mut inner, path.clone()).is_err());
    assert_eq!(inner.project, before);
    assert!(!path.exists());
}

#[test]
fn starter_refuses_an_active_transaction_before_writing() {
    let dir = TempDir::new("starter-transaction");
    std::fs::create_dir_all(dir.path()).unwrap();
    let path = dir.path().join("Sketch.ocp");
    let state = AppState::new();
    let mut inner = state.lock_inner();
    inner
        .begin_transaction("agent batch".into(), "agent".into())
        .unwrap();
    assert!(crate::commands::create_starter_project_in(&mut inner, path.clone()).is_err());
    assert!(!path.exists());
    assert!(inner.transaction.is_some());
}
