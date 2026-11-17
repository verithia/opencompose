//! The per-resource locks a second process has to respect.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::commands::{ProjectError, create_project_in, open_project};
use crate::state::AppState;

/// A separate open file description holding the lock, which is what a second
/// process presents to `flock`.
fn hold_elsewhere(path: &Path) -> File {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    crate::private_fs::lock(
        path,
        crate::private_fs::LockMode::Exclusive,
        crate::private_fs::LockWait::NoWait,
    )
    .unwrap()
}

fn project_file(dir: &Path, name: &str, source: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, source).unwrap();
    path
}

#[test]
fn opening_a_project_held_elsewhere_keeps_the_current_document() {
    let dir = tempfile::tempdir().unwrap();
    let source = "@project \"Current\"\n  tempo: 120\n";
    let current = project_file(dir.path(), "current.ocp", source);
    let target = project_file(dir.path(), "target.ocp", "@project \"Target\"\n");
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        open_project(&mut inner, source, Some(current.clone())).unwrap();
    }
    let before = state.lock_inner().dsl_source.clone();
    let target_before = std::fs::read(&target).unwrap();
    let _foreign = hold_elsewhere(&crate::project_data::project_lock_path(&target));

    let error = {
        let mut inner = state.lock_inner();
        open_project(&mut inner, "@project \"Other\"\n", Some(target.clone())).unwrap_err()
    };
    assert!(error.contains("Another OpenCompose process"), "{error}");
    let inner = state.lock_inner();
    assert_eq!(inner.file_path.as_deref(), Some(current.as_path()));
    assert_eq!(inner.dsl_source, before);
    assert_eq!(std::fs::read(&target).unwrap(), target_before);
}

#[test]
fn new_over_a_path_held_elsewhere_is_refused_before_it_writes() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("fresh.ocp");
    let _foreign = hold_elsewhere(&crate::project_data::project_lock_path(&target));
    let state = AppState::new();
    let error = {
        let mut inner = state.lock_inner();
        create_project_in(&mut inner, Some(target.clone())).unwrap_err()
    };
    assert!(matches!(error, ProjectError::ProjectInUse { .. }));
    assert!(!target.exists());
    assert!(state.lock_inner().file_path.is_none());
}

#[test]
fn a_process_reopens_the_project_it_already_holds() {
    let dir = tempfile::tempdir().unwrap();
    let source = "@project \"Again\"\n  tempo: 100\n";
    let path = project_file(dir.path(), "again.ocp", source);
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        open_project(&mut inner, source, Some(path.clone())).unwrap();
        open_project(&mut inner, source, Some(path.clone())).unwrap();
        assert_eq!(inner.file_path.as_deref(), Some(path.as_path()));
    }
}

#[test]
fn a_fresh_state_starts_without_a_project_lock() {
    let state = AppState::new();
    assert!(state.lock_inner().project_lock.is_none());
}
