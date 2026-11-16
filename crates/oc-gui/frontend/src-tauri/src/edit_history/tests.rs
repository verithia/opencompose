use std::path::{Path, PathBuf};

use super::journal::{self, Journal, StoredEntry};
use super::{Author, act_as, acting};
use crate::state::{AppState, AppStateInner, MAX_UNDO, UndoEdit, UndoScope, UndoScopes};

fn entry(id: u64) -> StoredEntry {
    StoredEntry {
        id,
        at_ms: id * 10,
        label: format!("edit {id}"),
        subject: None,
        author: Author::Person,
        scopes: UndoScopes::one(UndoScope::Project),
        forward: vec![id as u8; 3],
        inverse: vec![0, id as u8],
    }
}

fn project_file(dir: &Path) -> PathBuf {
    dir.join("song.ocp")
}

/// A state whose open project is `song.ocp` in a fresh directory, with its
/// history kept in this test thread's history folder.
fn opened(dir: &Path, source: &str) -> AppState {
    super::enable_persistence_for_test();
    let path = project_file(dir);
    std::fs::write(&path, source).unwrap();
    let state = AppState::new();
    crate::commands::open_project(&mut state.lock_inner(), source, Some(path)).unwrap();
    state
}

fn set_tempo(inner: &mut AppStateInner, tempo: f64) {
    let old = inner.project.clone();
    inner.project.properties.tempo = tempo;
    crate::commands::shared::resync_with_undo(
        inner,
        old,
        UndoEdit::new("tempo", UndoScope::Transport).about(format!("{tempo}")),
    )
    .unwrap();
}

const SOURCE: &str = "@project \"Song\"\n  tempo: 100\n";

#[test]
fn journal_appends_truncates_and_reopens_with_its_cursor_and_head() {
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(dir.path());
    let mut journal = Journal::open(&path).unwrap();
    assert_eq!(journal.dir(), journal::dir_for(&path));
    journal.append(&[entry(1), entry(2), entry(3)]).unwrap();
    journal.write_state(2, "head two").unwrap();
    journal.truncate(2).unwrap();
    journal.append(&[entry(9)]).unwrap();
    drop(journal);

    let journal = Journal::open(&path).unwrap();
    assert_eq!(journal.len(), 3);
    assert_eq!(journal.applied(), 2);
    assert_eq!(journal.head(), Some("head two"));
    let ids: Vec<u64> = journal
        .read_range(0..3)
        .unwrap()
        .iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(ids, [1, 2, 9]);
    assert_eq!(journal.read_range(1..2).unwrap()[0], entry(2));
    assert_eq!(journal.max_id(), 9);
}

#[test]
fn a_torn_index_tail_is_dropped_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(dir.path());
    let mut journal = Journal::open(&path).unwrap();
    journal.append(&[entry(1), entry(2)]).unwrap();
    drop(journal);
    // A crash between the entries write and the index write of a later
    // append, seen from the other side: an index row past the entries file.
    let entries = journal::dir_for(&path).join("entries.jsonl");
    let length = std::fs::metadata(&entries).unwrap().len();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&entries)
        .unwrap();
    file.set_len(length - 5).unwrap();
    let journal = Journal::open(&path).unwrap();
    assert_eq!(journal.len(), 1);
    assert_eq!(journal.read_range(0..1).unwrap()[0].id, 1);
}

#[test]
fn clear_older_keeps_the_newest_entries_and_never_an_undone_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(dir.path());
    let mut journal = Journal::open(&path).unwrap();
    journal
        .append(&(1..=6).map(entry).collect::<Vec<_>>())
        .unwrap();
    journal.write_state(4, "at four").unwrap();
    // Keeping one would drop undone entries 5 and 6: only the applied go.
    assert_eq!(journal.clear_older(1).unwrap(), 4);
    assert_eq!(journal.len(), 2);
    assert_eq!(journal.applied(), 0);
    assert_eq!(journal.head(), Some("at four"));
    drop(journal);
    let journal = Journal::open(&path).unwrap();
    let ids: Vec<u64> = journal
        .read_range(0..2)
        .unwrap()
        .iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(ids, [5, 6]);
    let retired = format!("{}.old", journal::dir_for(&path).display());
    assert!(!Path::new(&retired).exists());
}

#[test]
fn authors_follow_the_caller_and_the_acting_guard_restores() {
    assert_eq!(
        Author::from_caller(&crate::state::panel_batch_owner(Some("s1"))),
        Author::Agent {
            session: Some("s1".into())
        }
    );
    assert_eq!(
        Author::from_caller(&crate::state::panel_batch_owner(None)),
        Author::Agent { session: None }
    );
    assert_eq!(
        Author::from_caller(crate::state::HTTP_BATCH_OWNER),
        Author::Client {
            owner: "http".into()
        }
    );
    assert_eq!(Author::from_caller("gui"), Author::Person);
    assert_eq!(acting(), Author::Person);
    {
        let _outer = act_as(Author::Outside);
        {
            let _inner = act_as(Author::Agent { session: None });
            assert_eq!(acting(), Author::Agent { session: None });
        }
        assert_eq!(acting(), Author::Outside);
    }
    assert_eq!(acting(), Author::Person);
}

#[test]
fn edits_survive_a_reopen_and_undo_onto_the_saved_document() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    {
        let mut inner = state.lock_inner();
        set_tempo(&mut inner, 120.0);
        {
            let _agent = act_as(Author::Agent {
                session: Some("s1".into()),
            });
            set_tempo(&mut inner, 130.0);
        }
        inner.sync_history();
    }
    crate::commands::save_file_inner(None, &state).unwrap();
    drop(state);

    let reopened = AppState::new();
    let path = project_file(dir.path());
    let source = std::fs::read_to_string(&path).unwrap();
    let mut inner = reopened.lock_inner();
    crate::commands::open_project(&mut inner, &source, Some(path)).unwrap();
    let page = inner.undo.history_page(None, 64).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.applied, 2);
    assert_eq!(page.entries[0].subject.as_deref(), Some("130"));
    assert_eq!(
        page.entries[0].author,
        Author::Agent {
            session: Some("s1".into())
        }
    );
    assert_eq!(page.entries[1].author, Author::Person);
    inner.undo_step().unwrap();
    inner.undo_step().unwrap();
    assert_eq!(inner.project.properties.tempo, 100.0);
    inner.redo_step().unwrap();
    assert_eq!(inner.project.properties.tempo, 120.0);
}

#[test]
fn a_file_changed_outside_gets_one_bridging_entry() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    {
        let mut inner = state.lock_inner();
        set_tempo(&mut inner, 120.0);
        inner.sync_history();
    }
    drop(state);
    // The history recorded tempo 120; the file still says 100, as after a
    // Don't Save, and an editor elsewhere also renamed the project.
    let path = project_file(dir.path());
    let outside = "@project \"Renamed\"\n  tempo: 100\n";
    std::fs::write(&path, outside).unwrap();
    let reopened = AppState::new();
    let mut inner = reopened.lock_inner();
    crate::commands::open_project(&mut inner, outside, Some(path)).unwrap();
    let page = inner.undo.history_page(None, 64).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.entries[0].author, Author::Outside);
    assert_eq!(page.entries[0].name, "external change");
    inner.undo_step().unwrap();
    assert_eq!(inner.project.properties.tempo, 120.0);
    assert_eq!(inner.project.name.as_deref(), Some("Song"));
    inner.undo_step().unwrap();
    assert_eq!(inner.project.properties.tempo, 100.0);
}

#[test]
fn an_unchanged_file_reopens_without_a_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    {
        let mut inner = state.lock_inner();
        set_tempo(&mut inner, 120.0);
    }
    crate::commands::save_file_inner(None, &state).unwrap();
    drop(state);
    let path = project_file(dir.path());
    let source = std::fs::read_to_string(&path).unwrap();
    let reopened = AppState::new();
    let mut inner = reopened.lock_inner();
    crate::commands::open_project(&mut inner, &source, Some(path)).unwrap();
    assert_eq!(inner.undo.total_levels(), 1);
}

#[test]
fn the_window_pages_from_disk_and_memory_stays_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let mut inner = state.lock_inner();
    let edits = MAX_UNDO + 150;
    for step in 0..edits {
        set_tempo(&mut inner, 101.0 + step as f64);
        if step % 50 == 0 {
            inner.sync_history();
        }
    }
    inner.sync_history();
    assert_eq!(inner.undo.total_levels(), edits);
    assert!(inner.undo.undo.len() <= MAX_UNDO);
    for _ in 0..edits {
        inner.undo_step().unwrap();
        inner.sync_history();
        assert!(inner.undo.undo.len() + inner.undo.redo.len() <= 2 * MAX_UNDO);
    }
    assert_eq!(inner.project.properties.tempo, 100.0);
    assert!(inner.undo_step().is_err());
    for _ in 0..edits {
        inner.redo_step().unwrap();
        inner.sync_history();
    }
    assert_eq!(inner.project.properties.tempo, 100.0 + edits as f64);
}

#[test]
fn a_jump_beyond_the_window_streams_patches_and_lands_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let mut inner = state.lock_inner();
    let edits = MAX_UNDO + 100;
    for step in 0..edits {
        set_tempo(&mut inner, 101.0 + step as f64);
    }
    inner.sync_history();
    inner.restore_level(3).unwrap();
    assert_eq!(inner.project.properties.tempo, 103.0);
    assert_eq!(inner.undo.applied_level(), 3);
    inner.restore_level(edits - 1).unwrap();
    assert_eq!(inner.project.properties.tempo, 100.0 + (edits - 1) as f64);
    // A new edit after the jump drops the redo branch, on disk too.
    inner.restore_level(10).unwrap();
    set_tempo(&mut inner, 500.0);
    inner.sync_history();
    assert_eq!(inner.undo.total_levels(), 11);
    let path = project_file(dir.path());
    assert_eq!(Journal::open(&path).unwrap().len(), 11);
}

#[test]
fn a_failed_jump_leaves_the_document_and_the_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let mut inner = state.lock_inner();
    for step in 0..(MAX_UNDO + 20) {
        set_tempo(&mut inner, 101.0 + step as f64);
    }
    inner.sync_history();
    // Corrupt the oldest entry's inverse patch on disk.
    let entries = journal::dir_for(&project_file(dir.path())).join("entries.jsonl");
    let text = std::fs::read_to_string(&entries).unwrap();
    let first = text.lines().next().unwrap();
    let mut broken: serde_json::Value = serde_json::from_str(first).unwrap();
    let length = broken["inverse"].as_str().unwrap().len();
    broken["inverse"] = serde_json::json!("A".repeat(length));
    let replaced = serde_json::to_string(&broken).unwrap();
    assert_eq!(replaced.len(), first.len(), "the row lengths stay valid");
    std::fs::write(&entries, text.replacen(first, &replaced, 1)).unwrap();
    let before = inner.project.clone();
    let level = inner.undo.applied_level();
    assert!(inner.restore_level(0).is_err());
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.applied_level(), level);
}

#[test]
fn a_rolled_back_batch_leaves_no_entry_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    state
        .begin_transaction("turn".into(), crate::state::panel_batch_owner(Some("s1")))
        .unwrap();
    {
        let mut inner = state.lock_inner();
        set_tempo(&mut inner, 140.0);
        inner.sync_history();
    }
    let path = project_file(dir.path());
    assert_eq!(
        Journal::open(&path).unwrap().len(),
        0,
        "a batch is provisional"
    );
    state.rollback_transaction().unwrap();
    state.lock_inner().sync_history();
    assert_eq!(Journal::open(&path).unwrap().len(), 0);

    state
        .begin_transaction("turn".into(), crate::state::panel_batch_owner(Some("s1")))
        .unwrap();
    set_tempo(&mut state.lock_inner(), 150.0);
    state.commit_transaction().unwrap();
    let journal = Journal::open(&path).unwrap();
    assert_eq!(journal.len(), 1);
    let stored = journal.read_range(0..1).unwrap().remove(0);
    assert_eq!(
        stored.author,
        Author::Agent {
            session: Some("s1".into())
        }
    );
}

#[test]
fn save_as_copies_the_history_and_new_project_discards_one() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    set_tempo(&mut state.lock_inner(), 120.0);
    let copy = dir.path().join("copy.ocp");
    crate::commands::save_file_inner(Some(copy.display().to_string()), &state).unwrap();
    set_tempo(&mut state.lock_inner(), 125.0);
    state.lock_inner().sync_history();
    assert_eq!(Journal::open(&copy).unwrap().len(), 2);
    assert_eq!(Journal::open(&project_file(dir.path())).unwrap().len(), 1);

    crate::commands::create_project_in(&mut state.lock_inner(), Some(copy.clone())).unwrap();
    assert_eq!(state.lock_inner().undo.total_levels(), 0);
    assert_eq!(Journal::open(&copy).unwrap().len(), 0);
}

#[test]
fn a_first_save_writes_the_window_of_an_unsaved_document() {
    super::enable_persistence_for_test();
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new();
    set_tempo(&mut state.lock_inner(), 111.0);
    assert!(!state.lock_inner().undo.journaled());
    let path = project_file(dir.path());
    crate::commands::save_file_inner(Some(path.display().to_string()), &state).unwrap();
    assert!(state.lock_inner().undo.journaled());
    assert_eq!(Journal::open(&path).unwrap().len(), 1);
}

#[test]
fn history_is_not_written_unless_a_test_asks() {
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(dir.path());
    let result = std::thread::spawn(move || {
        std::fs::write(&path, SOURCE).unwrap();
        let state = AppState::new();
        crate::commands::open_project(&mut state.lock_inner(), SOURCE, Some(path.clone())).unwrap();
        set_tempo(&mut state.lock_inner(), 120.0);
        state.lock_inner().sync_history();
        journal::dir_for(&path).exists()
    })
    .join()
    .unwrap();
    assert!(!result);
}

#[test]
fn move_project_takes_the_history_along_in_the_same_folder_and_across() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    set_tempo(&mut state.lock_inner(), 120.0);
    let from = project_file(dir.path());
    let renamed = dir.path().join("renamed.ocp");
    crate::commands::move_project_inner(&state, renamed.clone()).unwrap();
    assert!(!from.exists() && !journal::dir_for(&from).exists());
    assert!(renamed.exists());
    assert_eq!(Journal::open(&renamed).unwrap().len(), 1);
    assert_eq!(
        state.lock_inner().file_path.as_deref(),
        Some(renamed.as_path())
    );

    let elsewhere = dir.path().join("other");
    std::fs::create_dir(&elsewhere).unwrap();
    let moved = elsewhere.join("moved.ocp");
    crate::commands::move_project_inner(&state, moved.clone()).unwrap();
    assert!(!renamed.exists() && !journal::dir_for(&renamed).exists());
    assert_eq!(Journal::open(&moved).unwrap().len(), 1);
    let mut inner = state.lock_inner();
    assert_eq!(inner.file_path.as_deref(), Some(moved.as_path()));
    assert!(!inner.dirty);
    inner.undo_step().unwrap();
    assert_eq!(inner.project.properties.tempo, 100.0);
}

#[test]
fn move_project_refuses_an_occupied_destination_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    set_tempo(&mut state.lock_inner(), 120.0);
    let taken = dir.path().join("taken.ocp");
    std::fs::write(&taken, "keep me").unwrap();
    assert!(crate::commands::move_project_inner(&state, taken.clone()).is_err());
    assert_eq!(std::fs::read_to_string(&taken).unwrap(), "keep me");
    let from = project_file(dir.path());
    assert!(from.exists());
    assert_eq!(
        state.lock_inner().file_path.as_deref(),
        Some(from.as_path())
    );
}

#[test]
fn a_merge_after_a_sync_rewrites_the_entry_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    {
        let mut inner = state.lock_inner();
        inner.undo.begin_gesture(UndoScope::Transport);
        set_tempo(&mut inner, 110.0);
        inner.sync_history();
        // The gesture goes on after the tick wrote its first step, and its
        // next step touches more than the first did.
        let old = inner.project.clone();
        inner.project.properties.tempo = 115.0;
        inner.project.name = Some("Dragged".into());
        crate::commands::shared::resync_with_undo(
            &mut inner,
            old,
            UndoEdit::new("tempo", UndoScope::Transport),
        )
        .unwrap();
        inner.undo.end_gesture();
        inner.sync_history();
    }
    drop(state);
    let path = project_file(dir.path());
    let source = std::fs::read_to_string(&path).unwrap();
    let reopened = AppState::new();
    let mut inner = reopened.lock_inner();
    crate::commands::open_project(&mut inner, &source, Some(path)).unwrap();
    // The file still says 100; the history ends at 115, so a bridge follows.
    assert_eq!(inner.undo.total_levels(), 2);
    inner.undo_step().unwrap();
    assert_eq!(inner.project.properties.tempo, 115.0);
    inner.undo_step().unwrap();
    assert_eq!(
        inner.project.properties.tempo, 100.0,
        "one entry holds the whole drag"
    );
    assert_eq!(inner.project.name.as_deref(), Some("Song"));
}

#[test]
fn the_history_lives_in_the_app_folder_with_a_marker_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    set_tempo(&mut state.lock_inner(), 120.0);
    let path = project_file(dir.path());
    crate::commands::save_file_inner(None, &state).unwrap();
    let history = journal::dir_for(&path);
    assert!(history.starts_with(journal::root()));
    assert_eq!(
        history.file_name().unwrap().to_str().unwrap(),
        crate::project_data::path_key(&path)
    );
    let marker = journal::read_marker(&history).unwrap();
    assert_eq!(marker.path, crate::project_data::canonical_path(&path));
    assert_eq!(marker.sha256, crate::project_data::content_sha256(&path));
    let beside: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .filter(|name| name != "song.ocp")
        .collect();
    assert!(
        beside.is_empty(),
        "nothing is written beside the project: {beside:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        state.lock_inner().undo.clear_older(0).ok();
        for entry in std::fs::read_dir(&history).unwrap().flatten() {
            let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(mode & 0o077, 0, "{:?} is private", entry.file_name());
        }
    }
}

fn reopen(path: &Path) -> AppState {
    let source = std::fs::read_to_string(path).unwrap();
    let state = AppState::new();
    crate::commands::open_project(&mut state.lock_inner(), &source, Some(path.to_path_buf()))
        .unwrap();
    state
}

#[test]
fn a_file_moved_outside_the_app_finds_its_history_by_content() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    set_tempo(&mut state.lock_inner(), 120.0);
    crate::commands::save_file_inner(None, &state).unwrap();
    drop(state);
    let from = project_file(dir.path());
    let moved_dir = dir.path().join("elsewhere");
    std::fs::create_dir(&moved_dir).unwrap();
    let to = moved_dir.join("renamed.ocp");
    std::fs::rename(&from, &to).unwrap();

    let state = reopen(&to);
    let mut inner = state.lock_inner();
    assert_eq!(inner.undo.total_levels(), 1, "the history came along");
    assert!(!journal::dir_for(&from).exists(), "and left the old key");
    inner.undo_step().unwrap();
    assert_eq!(inner.project.properties.tempo, 100.0);
}

#[test]
fn a_file_copied_outside_the_app_gets_a_copy_of_the_history() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    set_tempo(&mut state.lock_inner(), 120.0);
    crate::commands::save_file_inner(None, &state).unwrap();
    drop(state);
    let original = project_file(dir.path());
    let copy = dir.path().join("copy.ocp");
    std::fs::copy(&original, &copy).unwrap();

    assert_eq!(reopen(&copy).lock_inner().undo.total_levels(), 1);
    assert_eq!(reopen(&original).lock_inner().undo.total_levels(), 1);
}

#[test]
fn a_new_project_never_takes_a_history_by_content() {
    let dir = tempfile::tempdir().unwrap();
    super::enable_persistence_for_test();
    let state = AppState::new();
    let first = dir.path().join("first.ocp");
    crate::commands::create_project_in(&mut state.lock_inner(), Some(first.clone())).unwrap();
    set_tempo(&mut state.lock_inner(), 130.0);
    crate::commands::save_file_inner(None, &state).unwrap();
    // The same bytes at another path, written by New: the file is new.
    let bytes = std::fs::read(&first).unwrap();
    std::fs::remove_file(&first).unwrap();
    let second = dir.path().join("second.ocp");
    crate::commands::create_project_in(&mut state.lock_inner(), Some(second.clone())).unwrap();
    std::fs::write(&second, &bytes).unwrap();
    assert_eq!(
        reopen(&second).lock_inner().undo.total_levels(),
        1,
        "only the bridge"
    );
    assert!(
        journal::dir_for(&first).exists(),
        "the old history stays for its file"
    );
}
