//! Durable writes of an agent edit: the project file, the single `.bak`, and
//! the exit flush.
//!
//! Every case drives the command bodies the panel calls. A write happens only
//! for a document the save path would accept: a preview, an open batch and a
//! project with no path are left unsaved, and a failed write leaves the file
//! and its directory as they were.

use std::path::{Path, PathBuf};

use crate::commands::ai::apply_edit_to_project;
use crate::commands::preview::{accept_inner, begin_inner, cancel_inner};
use crate::commands::shared::resync_with_undo;
use crate::project_persist::{after_tool, backup_path, flush_exit, flush_state, logged};
use crate::state::{AppState, UndoEdit, UndoScope};
use serde_json::json;

use super::support::{TempDir, make_track, state_with_project};

const SONG: &str = r#"@ocp "0.2.0"

@project "agent song"
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

/// The same pattern with one extra note: a proposal the preview can hold.
const RIFF_EXTRA: &str = r#"@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
"#;

struct Fixture {
    dir: TempDir,
    state: AppState,
    path: PathBuf,
}

impl Fixture {
    /// An open dirty project whose file already holds the baseline document.
    fn new(label: &str) -> Self {
        let dir = TempDir::new(label);
        std::fs::create_dir_all(dir.path()).unwrap();
        let state = state_with_project(SONG, dir.path());
        let baseline = {
            let mut inner = state.lock_inner();
            oc_dsl::instruments::apply_defaults(&mut inner.project);
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.dirty = true;
            inner.dsl_source.clone()
        };
        let path = dir.path().join("song.ocp");
        std::fs::write(&path, &baseline).unwrap();
        Self { dir, state, path }
    }

    fn bytes(&self) -> Vec<u8> {
        std::fs::read(&self.path).unwrap()
    }

    fn backup(&self) -> PathBuf {
        backup_path(&self.path)
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

/// Apply the agent's project-level replacement with `count` tracks.
fn add_tracks(state: &AppState, count: usize) -> crate::commands::ProjectSnapshot {
    let (dsl, expected) = {
        let inner = state.lock_inner();
        let mut project = inner.project.clone();
        project.tracks.truncate(1);
        for index in 1..count {
            project.tracks.push(make_track(&format!("pad{index}")));
        }
        (oc_dsl::serialize(&project), inner.dsl_source.clone())
    };
    let mut inner = state.lock_inner();
    apply_edit_to_project(&mut inner, dsl, "project".into(), expected).expect("the edit applies")
}

/// One document edit with no agent write behind it, as a command body makes it.
fn set_mute(state: &AppState, mute: bool) -> String {
    let mut inner = state.lock_inner();
    let old = inner.project.clone();
    inner.project.tracks[0].mix.mute = mute;
    resync_with_undo(
        &mut inner,
        old,
        UndoEdit::new("mute", UndoScope::MixerLevels),
    )
    .unwrap();
    inner.dsl_source.clone()
}

fn names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn an_agent_applied_edit_writes_the_project_file() {
    let fixture = Fixture::new("agent-write");
    let before = fixture.bytes();

    let applied = add_tracks(&fixture.state, 2);

    let after = fixture.bytes();
    assert_ne!(after, before, "the applied edit reached the file");
    let on_disk = oc_dsl::parse(std::str::from_utf8(&after).unwrap()).expect("the file parses");
    assert_eq!(on_disk.tracks.len(), 2, "the file holds the new track");
    assert_eq!(applied.track_count, 2);
    assert_eq!(after, fixture.state.lock_inner().dsl_source.as_bytes());

    let path = fixture.path.display().to_string();
    assert!(
        logged().iter().any(|line| line.starts_with("info:")
            && line.contains(&path)
            && line.contains(&format!("({} bytes)", after.len()))),
        "the write reports the path and its byte count: {:?}",
        logged()
    );
}

#[test]
fn the_backup_holds_the_content_the_write_replaced() {
    let fixture = Fixture::new("agent-backup");
    let baseline = fixture.bytes();

    add_tracks(&fixture.state, 2);
    let first = fixture.bytes();
    assert_eq!(
        std::fs::read(fixture.backup()).unwrap(),
        baseline,
        "the first write keeps what it replaced"
    );

    add_tracks(&fixture.state, 3);
    assert_eq!(
        std::fs::read(fixture.backup()).unwrap(),
        first,
        "the next write overwrites the one backup"
    );
    let names = fixture.names();
    assert_eq!(
        names.iter().filter(|name| name.ends_with(".bak")).count(),
        1,
        "one backup, not a growing set: {names:?}"
    );
    assert!(
        names.iter().all(|name| !name.ends_with(".tmp")),
        "{names:?}"
    );
}

#[test]
fn an_accepted_preview_writes_the_proposal_to_the_file() {
    let fixture = Fixture::new("agent-accept");
    let baseline = fixture.bytes();
    let base = fixture.state.lock_inner().dsl_source.clone();
    {
        let mut inner = fixture.state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_EXTRA.to_string(),
            "pattern:riff".into(),
            base,
        )
        .expect("the proposal applies");
    }
    assert_eq!(fixture.bytes(), baseline, "a preview is not the document");

    let accepted = {
        let mut inner = fixture.state.lock_inner();
        accept_inner(&mut inner).expect("accept commits the proposal")
    };
    assert_eq!(fixture.bytes(), accepted.dsl_source.as_bytes());
    assert_eq!(std::fs::read(fixture.backup()).unwrap(), baseline);
}

#[test]
fn a_rejected_declined_reverted_or_rolled_back_edit_writes_nothing() {
    let fixture = Fixture::new("agent-declined");
    let baseline = fixture.bytes();

    // Rejected: the replacement does not apply, so there is no document to write.
    let expected = fixture.state.lock_inner().dsl_source.clone();
    let error = {
        let mut inner = fixture.state.lock_inner();
        apply_edit_to_project(
            &mut inner,
            "@track keys\n  bogus: 1\n".into(),
            "track:keys".into(),
            expected,
        )
        .unwrap_err()
    };
    assert!(!error.is_empty());
    assert_eq!(fixture.bytes(), baseline, "a rejected edit writes nothing");
    assert!(!fixture.backup().exists());

    // Declined: neither side of a preview is the document.
    let base = fixture.state.lock_inner().dsl_source.clone();
    {
        let mut inner = fixture.state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_EXTRA.to_string(),
            "pattern:riff".into(),
            base,
        )
        .unwrap();
        cancel_inner(&mut inner).expect("decline restores the base");
    }
    assert_eq!(fixture.bytes(), baseline);
    assert!(!fixture.backup().exists());

    // Reverted: the applied document is on disk; history restores older content
    // to the document, not to the file.
    add_tracks(&fixture.state, 2);
    let applied = fixture.bytes();
    assert_ne!(applied, baseline);
    let backup_after_apply = std::fs::read(fixture.backup()).unwrap();
    {
        let mut inner = fixture.state.lock_inner();
        inner.undo_step().expect("the applied edit undoes");
    }
    after_tool(&fixture.state, "undo", &mut json!({}));
    after_tool(&fixture.state, "redo", &mut json!({}));
    assert_eq!(fixture.bytes(), applied, "undo and redo do not write");
    assert_eq!(std::fs::read(fixture.backup()).unwrap(), backup_after_apply);

    // Rolled back: a batch that is discarded was never published.
    {
        let mut inner = fixture.state.lock_inner();
        inner
            .begin_transaction("turn".into(), "test".into())
            .unwrap();
    }
    set_mute(&fixture.state, true);
    after_tool(&fixture.state, "tracks_mix", &mut json!({}));
    assert_eq!(
        fixture.bytes(),
        applied,
        "an open batch is not the file's document"
    );
    {
        let mut inner = fixture.state.lock_inner();
        inner.rollback_transaction().unwrap();
    }
    after_tool(&fixture.state, "transaction_rollback", &mut json!({}));
    assert_eq!(fixture.bytes(), applied);
    assert_eq!(
        std::fs::read(fixture.backup()).unwrap(),
        backup_after_apply,
        "a discarded batch replaced nothing"
    );
    assert!(
        fixture.names().iter().all(|name| !name.ends_with(".tmp")),
        "{:?}",
        fixture.names()
    );
}

#[test]
fn a_mutating_tool_publishes_the_file_and_a_batch_waits_for_its_commit() {
    let fixture = Fixture::new("agent-tool");
    let baseline = fixture.bytes();

    // A call that published the document already on disk writes nothing.
    after_tool(&fixture.state, "tracks_mix", &mut json!({}));
    assert_eq!(fixture.bytes(), baseline);
    assert!(!fixture.backup().exists());

    // A mutating tool body publishes.
    let muted = set_mute(&fixture.state, true);
    after_tool(&fixture.state, "tracks_mix", &mut json!({}));
    assert_eq!(fixture.bytes(), muted.as_bytes());

    // The panel's turn batch holds its edits until the commit publishes them.
    {
        let mut inner = fixture.state.lock_inner();
        inner
            .begin_transaction("turn".into(), "test".into())
            .unwrap();
    }
    let unmuted = set_mute(&fixture.state, false);
    after_tool(&fixture.state, "tracks_mix", &mut json!({}));
    assert_eq!(
        fixture.bytes(),
        muted.as_bytes(),
        "an open batch is not published"
    );
    {
        let mut inner = fixture.state.lock_inner();
        inner.commit_transaction().unwrap();
    }
    after_tool(&fixture.state, "transaction_commit", &mut json!({}));
    assert_eq!(fixture.bytes(), unmuted.as_bytes());
}

#[test]
fn a_batch_a_dead_turn_left_open_is_committed_and_the_file_saves() {
    let fixture = Fixture::new("agent-batch-recover");
    let baseline = fixture.state.lock_inner().dsl_source.clone();
    let path = fixture.path.clone();
    {
        let mut inner = fixture.state.lock_inner();
        inner
            .begin_transaction("turn".into(), "panel:s1".into())
            .unwrap();
    }
    set_mute(&fixture.state, true);
    assert!(fixture.state.lock_inner().transaction.is_some());

    // The panel mounts and recovers the batch the dead turn left open: the
    // applied edit stays, and the project file holds it.
    let recovered = crate::commands::recover_panel_batch(&fixture.state)
        .unwrap()
        .expect("the panel's orphan is recovered");
    assert!(fixture.state.lock_inner().transaction.is_none());
    assert_ne!(recovered.dsl_source, baseline);
    assert!(recovered.project.tracks[0].mix.mute);
    assert_eq!(fixture.bytes(), recovered.dsl_source.as_bytes());

    // Save works immediately, and a second recovery is a no-op.
    assert_eq!(
        crate::commands::save_file_inner(None, &fixture.state).unwrap(),
        path.display().to_string()
    );
    assert!(
        crate::commands::recover_panel_batch(&fixture.state)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_project_with_no_path_stays_unsaved() {
    let dir = TempDir::new("agent-untitled");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = state_with_project(SONG, dir.path());
    {
        let mut inner = state.lock_inner();
        inner.file_path = None;
        oc_dsl::instruments::apply_defaults(&mut inner.project);
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.dirty = true;
    }

    let applied = add_tracks(&state, 2);
    assert_eq!(applied.file_path, None);
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "no filename is invented"
    );
    assert!(
        logged()
            .iter()
            .any(|line| line.starts_with("warn:") && line.contains("no file path")),
        "{:?}",
        logged()
    );
    // A project with no file has nowhere to publish, and the caller is told.
    let refused = flush_state(&state).unwrap_err();
    assert!(refused.contains("no file path"), "{refused}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn the_exit_flush_writes_a_dirty_project_once_and_leaves_a_clean_one_alone() {
    let fixture = Fixture::new("agent-exit");
    let baseline = fixture.bytes();

    // An edit no agent route published: only the exit flush can reach the file.
    let edited = set_mute(&fixture.state, true);
    assert_eq!(fixture.bytes(), baseline);
    assert_eq!(flush_state(&fixture.state).unwrap(), Some(edited.len()));
    assert_eq!(fixture.bytes(), edited.as_bytes());
    assert_eq!(std::fs::read(fixture.backup()).unwrap(), baseline);

    // A second exit event has nothing left to publish.
    assert!(flush_state(&fixture.state).unwrap().is_none());
    assert_eq!(fixture.bytes(), edited.as_bytes());

    // A clean project is left alone even when the document moved on in memory.
    {
        let mut inner = fixture.state.lock_inner();
        inner.dsl_source = format!("{edited}\n# scratch\n");
        inner.dirty = false;
    }
    assert!(flush_state(&fixture.state).unwrap().is_none());
    assert_eq!(fixture.bytes(), edited.as_bytes());
}

/// The write half of a flush runs without the document lock.
///
/// A FIFO at the project path blocks the read that publication starts with.
/// Opening its writer end returns only once the publisher is inside that read,
/// so the peer can take the document lock exactly while the write is in
/// progress, which proves publication does not hold the lock across its I/O.
#[cfg(unix)]
#[test]
fn a_publish_does_not_hold_the_document_lock() {
    use std::io::Write;

    let dir = TempDir::new("agent-lock-free");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = std::sync::Arc::new(state_with_project(SONG, dir.path()));
    let fifo = dir.path().join("song.ocp");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success(), "mkfifo failed");
    let source = {
        let mut inner = state.lock_inner();
        inner.file_path = Some(fifo.clone());
        inner.dirty = true;
        inner.dsl_source.clone()
    };

    let writer = {
        let state = std::sync::Arc::clone(&state);
        let fifo = fifo.clone();
        std::thread::spawn(move || {
            let mut pipe = std::fs::OpenOptions::new().write(true).open(&fifo).unwrap();
            let lock_free = state.try_lock_inner().is_some();
            pipe.write_all(source.as_bytes()).unwrap();
            drop(pipe);
            lock_free
        })
    };

    let published = flush_state(&state);
    let lock_free = writer.join().unwrap();

    assert!(
        lock_free,
        "the document lock must be free while the project file is written"
    );
    assert_eq!(
        published.unwrap(),
        None,
        "the file already held the document, so nothing was written"
    );
}

#[cfg(unix)]
#[test]
fn a_failed_write_leaves_no_temp_file_and_logs_the_reason() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new("agent-readonly");
    let baseline = fixture.bytes();
    // A document the flush must try to publish.
    set_mute(&fixture.state, true);
    assert_ne!(
        fixture.state.lock_inner().dsl_source.as_bytes(),
        baseline.as_slice()
    );

    std::fs::set_permissions(fixture.dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let written = flush_state(&fixture.state);
    // Restore the mode before the fixture removes the tree.
    std::fs::set_permissions(fixture.dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        written.is_err(),
        "a read-only directory refuses the write: {written:?}"
    );
    assert_eq!(
        fixture.bytes(),
        baseline,
        "the file keeps the last good content"
    );
    let names = names(fixture.dir.path());
    assert!(
        names.iter().all(|name| !name.ends_with(".tmp")),
        "no temporary file survives: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name.ends_with(".bak")),
        "no backup of a file the write did not replace: {names:?}"
    );
    let path = fixture.path.display().to_string();
    assert!(
        logged().iter().any(|line| line.starts_with("warn:")
            && line.contains(&path)
            && line.contains("could not")),
        "{:?}",
        logged()
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_project_is_refused_before_anything_is_written() {
    let fixture = Fixture::new("agent-symlink");
    let baseline = fixture.bytes();
    let link = fixture.dir.path().join("linked.ocp");
    std::os::unix::fs::symlink(&fixture.path, &link).unwrap();
    {
        let mut inner = fixture.state.lock_inner();
        inner.file_path = Some(link.clone());
    }
    set_mute(&fixture.state, true);

    assert!(flush_state(&fixture.state).is_err());
    assert_eq!(fixture.bytes(), baseline, "the target is untouched");
    assert!(
        !backup_path(&link).exists(),
        "no backup beside a link the save path refuses"
    );
    let path = link.display().to_string();
    assert!(
        logged().iter().any(|line| line.starts_with("warn:")
            && line.contains(&path)
            && line.contains("symlink")),
        "{:?}",
        logged()
    );
}

#[cfg(unix)]
#[test]
fn a_failed_agent_write_is_reported_in_the_tool_result() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new("agent-stale-result");
    set_mute(&fixture.state, true);
    std::fs::set_permissions(fixture.dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut result = json!({"applied": true, "revision": 4, "track_count": 2});
    after_tool(&fixture.state, "tracks_mix", &mut result);
    std::fs::set_permissions(fixture.dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    // The model reads the tool result, so the stale file has to be in it.
    assert_eq!(result["project_file_saved"], json!(false));
    let error = result["project_file_error"]
        .as_str()
        .expect("the result names the reason");
    assert!(error.contains("was not written"), "{error}");
    // The applied document stays the result's own payload.
    assert_eq!(result["applied"], json!(true));
    assert_eq!(result["revision"], json!(4));

    // A tool that does not publish leaves the result alone.
    let mut untouched = json!({"applied": true});
    after_tool(&fixture.state, "undo", &mut untouched);
    assert_eq!(untouched, json!({"applied": true}));
}

#[test]
fn a_clean_previewed_or_open_document_reports_success_with_no_bytes() {
    let fixture = Fixture::new("agent-no-write");
    let base = fixture.state.lock_inner().dsl_source.clone();
    {
        let mut inner = fixture.state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_EXTRA.to_string(),
            "pattern:riff".into(),
            base,
        )
        .expect("the proposal applies");
    }
    assert_eq!(
        flush_state(&fixture.state).unwrap(),
        None,
        "an open preview writes nothing"
    );
    {
        let mut inner = fixture.state.lock_inner();
        cancel_inner(&mut inner).expect("decline restores the base");
        inner
            .begin_transaction("turn".into(), "test".into())
            .unwrap();
    }
    set_mute(&fixture.state, true);
    assert_eq!(
        flush_state(&fixture.state).unwrap(),
        None,
        "an open batch writes nothing"
    );
    {
        let mut inner = fixture.state.lock_inner();
        inner.rollback_transaction().unwrap();
        inner.dirty = false;
    }
    assert_eq!(
        flush_state(&fixture.state).unwrap(),
        None,
        "a clean document writes nothing"
    );

    let mut result = json!({"applied": true});
    after_tool(&fixture.state, "tracks_add", &mut result);
    assert_eq!(
        result,
        json!({"applied": true}),
        "a result with nothing to publish is unchanged"
    );
}

#[cfg(unix)]
#[test]
fn a_failed_exit_flush_logs_an_error_and_retries_once() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new("agent-exit-error");
    let baseline = fixture.bytes();
    set_mute(&fixture.state, true);

    std::fs::set_permissions(fixture.dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    flush_exit(&fixture.state);
    std::fs::set_permissions(fixture.dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    let errors: Vec<String> = logged()
        .into_iter()
        .filter(|line| line.starts_with("error:") && line.contains("on exit"))
        .collect();
    assert_eq!(errors.len(), 2, "one attempt and one retry: {errors:?}");
    assert!(
        !errors[0].contains("retry"),
        "the first line is the attempt: {errors:?}"
    );
    assert!(
        errors[1].contains("after one retry"),
        "the second line is the retry: {errors:?}"
    );
    assert_eq!(
        fixture.bytes(),
        baseline,
        "a refused exit write leaves the file alone"
    );

    // An exit write that can be made publishes instead of logging, so the error
    // level is reserved for a failure on both attempts.
    flush_exit(&fixture.state);
    assert_eq!(
        logged()
            .iter()
            .filter(|line| line.starts_with("error:") && line.contains("on exit"))
            .count(),
        2,
        "a successful exit write logs nothing at error level"
    );
    assert_ne!(fixture.bytes(), baseline);
}
