use std::path::{Path, PathBuf};

use oc_dsl::change::Change;

use super::journal::{self, Journal, StoredEntry};
use super::{Author, EditCause, MAX_OUTSIDE_ENTRIES, act_as, acting};
use crate::state::{AppState, AppStateInner, MAX_UNDO, UndoEdit, UndoScope, UndoScopes, UndoStack};

fn entry(id: u64) -> StoredEntry {
    StoredEntry {
        id,
        at_ms: id * 10,
        cause: super::EditCause::Edit,
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
        UndoEdit::new("tempo", UndoScope::Transport),
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
fn a_failed_index_write_rewinds_the_entries_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(dir.path());
    let mut journal = Journal::open(&path).unwrap();
    journal.append(&[entry(1)]).unwrap();
    let entries = journal::dir_for(&path).join("entries.jsonl");
    let before = std::fs::metadata(&entries).unwrap().len();
    // The index cannot be opened, so the append fails after writing its lines.
    let index = journal::dir_for(&path).join("index.bin");
    std::fs::remove_file(&index).unwrap();
    std::fs::create_dir(&index).unwrap();
    assert!(journal.append(&[entry(2)]).is_err());
    assert_eq!(
        std::fs::metadata(&entries).unwrap().len(),
        before,
        "the entries file kept its length"
    );
    // The rewind leaves the journal ready for the next append.
    std::fs::remove_dir(&index).unwrap();
    journal.append(&[entry(2)]).unwrap();
    assert_eq!(journal.len(), 2);
}

#[test]
fn a_failed_index_write_rewinds_the_index_and_the_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(dir.path());
    let mut journal = Journal::open(&path).unwrap();
    journal.append(&[entry(1)]).unwrap();
    let entries = journal::dir_for(&path).join("entries.jsonl");
    let index = journal::dir_for(&path).join("index.bin");
    let entries_before = std::fs::metadata(&entries).unwrap().len();
    let index_before = std::fs::metadata(&index).unwrap().len();
    journal.fail_next_index_write();
    assert!(journal.append(&[entry(2)]).is_err());
    assert_eq!(std::fs::metadata(&entries).unwrap().len(), entries_before);
    assert_eq!(std::fs::metadata(&index).unwrap().len(), index_before);
    journal.append(&[entry(2)]).unwrap();
    assert_eq!(journal.len(), 2);
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
fn a_state_whose_cursor_moved_follows_the_entry_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(dir.path());
    let mut journal = Journal::open(&path).unwrap();
    journal.append(&[entry(1), entry(2), entry(3)]).unwrap();
    journal.write_state(2, "head at two").unwrap();
    drop(journal);
    // A drop of the oldest index row moved entry 2 to the first position.
    let index = journal::dir_for(&path).join("index.bin");
    let rows = std::fs::read(&index).unwrap();
    std::fs::write(&index, &rows[journal::INDEX_ROW..]).unwrap();
    let journal = Journal::open(&path).unwrap();
    assert_eq!(journal.applied(), 1, "the cursor followed entry 2");
    assert_eq!(
        journal.head(),
        Some("head at two"),
        "the history and its document survived"
    );
}

#[test]
fn a_state_whose_cursor_is_gone_drops_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(dir.path());
    let mut journal = Journal::open(&path).unwrap();
    journal.append(&[entry(1), entry(2), entry(3)]).unwrap();
    journal.write_state(2, "head at two").unwrap();
    drop(journal);
    // The index lost entry 2 entirely, so the head names no row.
    let index = journal::dir_for(&path).join("index.bin");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&index)
        .unwrap();
    file.set_len(journal::INDEX_ROW as u64).unwrap();
    let journal = Journal::open(&path).unwrap();
    assert_eq!(journal.applied(), 1);
    assert_eq!(
        journal.head(),
        None,
        "a head whose cursor row is gone cannot anchor undo"
    );
}

#[test]
fn an_interrupted_clear_restores_its_history_and_sweeps_staging() {
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(dir.path());
    let mut journal = Journal::open(&path).unwrap();
    journal.append(&[entry(1)]).unwrap();
    journal.write_state(1, "head one").unwrap();
    drop(journal);
    let history = journal::dir_for(&path);
    let retired = PathBuf::from(format!("{}.old", history.display()));
    let staging = PathBuf::from(format!("{}.new", history.display()));
    std::fs::rename(&history, &retired).unwrap();
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("entries.jsonl"), "stale").unwrap();

    let journal = Journal::open(&path).unwrap();
    assert_eq!(journal.len(), 1, "the retired history came back");
    assert_eq!(journal.head(), Some("head one"));
    assert!(!retired.exists(), "the retired directory was swept");
    assert!(!staging.exists(), "the staging directory was swept");
}

#[test]
fn an_incomplete_copy_staging_is_not_promoted() {
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(dir.path());
    let mut journal = Journal::open(&path).unwrap();
    journal.append(&[entry(1)]).unwrap();
    journal.write_state(1, "head one").unwrap();
    drop(journal);
    let history = journal::dir_for(&path);
    let staging = PathBuf::from(format!("{}.new", history.display()));
    std::fs::rename(&history, &staging).unwrap();
    // Only a complete `.old` may be promoted; a crashed copy is removed.
    let journal = Journal::open(&path).unwrap();
    assert_eq!(journal.len(), 0);
    assert_eq!(journal.head(), None);
    assert!(!staging.exists(), "the incomplete staging was swept");
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
    let page = inner
        .undo
        .history_page(None, 64, &inner.project.properties)
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.applied, 2);
    assert_eq!(
        page.entries[0].changes.changes,
        vec![oc_dsl::change::Change::Tempo {
            from: 120.0,
            to: 130.0
        }]
    );
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
    let page = inner
        .undo
        .history_page(None, 64, &inner.project.properties)
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.entries[0].author, Author::Outside);
    assert_eq!(
        page.entries[0].changes.changes,
        vec![
            oc_dsl::change::Change::ProjectRenamed {
                from: Some("Song".into()),
                to: Some("Renamed".into()),
            },
            oc_dsl::change::Change::Tempo {
                from: 120.0,
                to: 100.0,
            },
        ]
    );
    inner.undo_step().unwrap();
    assert_eq!(inner.project.properties.tempo, 120.0);
    assert_eq!(inner.project.name.as_deref(), Some("Song"));
    inner.undo_step().unwrap();
    assert_eq!(inner.project.properties.tempo, 100.0);
}

#[test]
fn a_file_without_a_version_header_reopens_without_a_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let path = project_file(dir.path());
    {
        let mut inner = state.lock_inner();
        set_tempo(&mut inner, 132.0);
        // A restored snapshot can omit the version; the history records the
        // document exactly as it is installed.
        let old = inner.project.clone();
        inner.project.ocp_version = None;
        crate::commands::shared::resync_with_undo(
            &mut inner,
            old,
            UndoEdit::new("revert conversation", UndoScope::Project),
        )
        .unwrap();
        assert!(!inner.dsl_source.contains("@ocp"));
        std::fs::write(&path, &inner.dsl_source).unwrap();
        inner.sync_history();
    }
    drop(state);
    let source = std::fs::read_to_string(&path).unwrap();
    let reopened = AppState::new();
    let mut inner = reopened.lock_inner();
    crate::commands::open_project(&mut inner, &source, Some(path)).unwrap();
    let page = inner
        .undo
        .history_page(None, 64, &inner.project.properties)
        .unwrap();
    assert_eq!((page.total, page.applied), (2, 2));
    assert!(
        page.entries
            .iter()
            .all(|entry| entry.author != Author::Outside)
    );
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
fn a_window_reload_keeps_memory_when_the_journal_did_not_sync() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let mut inner = state.lock_inner();
    let edits = MAX_UNDO + 20;
    for step in 0..edits {
        set_tempo(&mut inner, 101.0 + step as f64);
    }
    inner.sync_history();
    for _ in 0..MAX_UNDO {
        inner.undo_step().unwrap();
    }
    assert!(inner.undo.undo.is_empty());
    let applied = inner.undo.applied_level();
    assert!(applied > 0, "the window has entries below it");
    // The cursor write fails, so the journal is behind the document.
    let state_file = journal::dir_for(&project_file(dir.path())).join("state.json");
    std::fs::remove_file(&state_file).unwrap();
    std::fs::create_dir(&state_file).unwrap();

    let before = inner.project.clone();
    assert!(
        inner.undo_step().is_err(),
        "a stale journal is not reloaded"
    );
    assert_eq!(inner.project, before);
    assert_eq!(inner.undo.applied_level(), applied);
    assert!(inner.undo.undo.is_empty(), "the window stayed");
    assert_eq!(inner.undo.redo.len(), MAX_UNDO);
}

#[test]
fn clearing_older_refuses_a_window_the_journal_did_not_receive() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let mut inner = state.lock_inner();
    set_tempo(&mut inner, 120.0);
    assert_eq!(inner.undo.undo.len(), 1);
    assert!(
        inner.undo.clear_older(0).is_err(),
        "an unsynced entry cannot be dropped"
    );
    assert_eq!(inner.undo.undo.len(), 1, "the entry stayed in memory");
}

#[test]
fn clearing_older_reports_a_failed_append() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let mut inner = state.lock_inner();
    let index = journal::dir_for(&project_file(dir.path())).join("index.bin");
    std::fs::create_dir_all(&index).unwrap();
    set_tempo(&mut inner, 120.0);
    let outcome = crate::commands::clear_older_history(&mut inner, 0).unwrap();
    let crate::commands::HistoryClearOutcome::Failed { message } = outcome else {
        panic!("a failed append is not a clear");
    };
    assert!(
        message.contains("edit history"),
        "the real write failure is reported: {message}"
    );
    assert_eq!(inner.undo.total_levels(), 1, "the entry stayed");
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
        let _acting = crate::edit_history::act_as(Author::Agent {
            session: Some("s1".into()),
        });
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
    {
        let mut inner = state.lock_inner();
        let _acting = crate::edit_history::act_as(Author::Agent {
            session: Some("s1".into()),
        });
        set_tempo(&mut inner, 150.0);
    }
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
fn a_batch_keeps_a_person_edit_between_agent_runs() {
    let agent = Author::Agent {
        session: Some("s1".into()),
    };
    for journaled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut stack = stack(journaled, dir.path());
        let mut project = tempo(100);
        let mark = stack.mark();
        let batch = stack.rollback_mark();
        let old = project.clone();
        for (tempo_value, author) in [
            (110, agent.clone()),
            (125, Author::Person),
            (140, agent.clone()),
        ] {
            let before = project.clone();
            project = tempo(tempo_value);
            push_edit(&mut stack, &before, &project, author);
            if journaled {
                stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
            }
        }
        stack.squash_since(
            batch,
            old,
            &project,
            UndoEdit::new("turn", UndoScope::Project).by(agent.clone()),
        );
        let authors: Vec<Author> = stack
            .undo
            .iter()
            .map(|entry| entry.author.clone())
            .collect();
        assert_eq!(
            authors,
            vec![agent.clone(), Author::Person, agent.clone()],
            "the person's entry keeps its author and place"
        );
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
            let journal = Journal::open(&project_file(dir.path())).unwrap();
            let stored = journal.read_range(0..3).unwrap();
            let authors: Vec<Author> = stored.into_iter().map(|entry| entry.author).collect();
            assert_eq!(
                authors,
                vec![agent.clone(), Author::Person, agent.clone()],
                "the journal keeps the foreign entry"
            );
        }
        let mut cursor = project.clone();
        for expected in [125, 110, 100] {
            stack.undo(&mut cursor).unwrap();
            assert_eq!(cursor, tempo(expected));
        }
        for expected in [110, 125, 140] {
            stack.redo(&mut cursor).unwrap();
            assert_eq!(cursor, tempo(expected));
        }
        let changes = stack
            .since(&mark, "s1", &project.properties)
            .expect("the person's edit is outside news");
        assert_eq!(changes.entries.len(), 1);
        assert_eq!(changes.entries[0].author, Author::Person);
        assert_eq!(changes.own_undone, 0);
    }
}

#[test]
fn save_as_copies_the_history_and_new_starts_a_fresh_one() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    set_tempo(&mut state.lock_inner(), 120.0);
    let copy = dir.path().join("copy.ocp");
    crate::commands::save_file_inner(Some(copy.display().to_string()), &state).unwrap();
    set_tempo(&mut state.lock_inner(), 125.0);
    state.lock_inner().sync_history();
    assert_eq!(Journal::open(&copy).unwrap().len(), 2);
    assert_eq!(Journal::open(&project_file(dir.path())).unwrap().len(), 1);

    // New refuses a path that holds a file, so the copy keeps its history.
    assert!(
        crate::commands::create_project_in(&mut state.lock_inner(), Some(copy.clone())).is_err()
    );
    assert_eq!(Journal::open(&copy).unwrap().len(), 2);
    let fresh = dir.path().join("fresh.ocp");
    crate::commands::create_project_in(&mut state.lock_inner(), Some(fresh.clone())).unwrap();
    assert_eq!(state.lock_inner().undo.total_levels(), 0);
    assert_eq!(Journal::open(&fresh).unwrap().len(), 0);
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
fn every_project_write_records_the_files_fingerprint() {
    super::enable_persistence_for_test();
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new();
    let path = project_file(dir.path());
    {
        let mut inner = state.lock_inner();
        crate::commands::create_project_in(&mut inner, Some(path.clone())).unwrap();
        let marker = journal::read_marker(&journal::dir_for(&path)).unwrap();
        assert_eq!(
            marker.sha256,
            journal::fingerprint(&path),
            "New records the file it wrote"
        );
        set_tempo(&mut inner, 140.0);
        crate::project_persist::flush(&mut inner).unwrap();
    }
    let marker = journal::read_marker(&journal::dir_for(&path)).unwrap();
    assert_eq!(
        marker.sha256,
        journal::fingerprint(&path),
        "a publish outside Save records the file it wrote"
    );
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
fn a_history_on_an_unmounted_volume_is_copied_not_moved() {
    super::enable_persistence_for_test();
    let dir = tempfile::tempdir().unwrap();
    let copy = project_file(dir.path());
    std::fs::write(&copy, SOURCE).unwrap();
    let sha = journal::fingerprint(&copy).unwrap();
    let unmounted = PathBuf::from("/Volumes/NotMounted-oc-history-test/song.ocp");
    assert!(!unmounted.exists(), "the fixture volume is not mounted");
    let mut journal = Journal::open(&unmounted).unwrap();
    journal.append(&[entry(1)]).unwrap();
    journal.write_state(1, SOURCE).unwrap();
    journal::write_marker(journal.dir(), &journal::Marker::new(&unmounted, Some(sha))).unwrap();
    drop(journal);

    let reopened = reopen(&copy);
    assert_eq!(reopened.lock_inner().undo.total_levels(), 1);
    assert!(
        journal::dir_for(&unmounted).exists(),
        "a file on an unmounted volume still owns its history"
    );
}

#[test]
fn an_exclusive_new_refuses_a_file_that_appeared() {
    super::enable_persistence_for_test();
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new();
    let target = dir.path().join("taken.ocp");
    std::fs::write(&target, "keep me").unwrap();
    let before = state.lock_inner().project.clone();
    let error = crate::commands::create_project_in(&mut state.lock_inner(), Some(target.clone()))
        .unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me");
    assert_eq!(state.lock_inner().project, before);
    assert_eq!(state.lock_inner().file_path, None);
}

#[test]
fn a_refused_new_leaves_the_targets_history_alone() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let target = dir.path().join("target.ocp");
    std::fs::write(&target, SOURCE).unwrap();
    let mut journal = Journal::open(&target).unwrap();
    journal.append(&[entry(1)]).unwrap();
    drop(journal);
    let mut inner = state.lock_inner();
    inner
        .begin_transaction("batch".into(), "owner".into())
        .unwrap();
    assert!(crate::commands::create_project_in(&mut inner, Some(target.clone())).is_err());
    assert_eq!(
        Journal::open(&target).unwrap().len(),
        1,
        "the refused New kept the target's history"
    );
}

#[test]
fn new_over_the_open_path_starts_a_history_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let path = project_file(dir.path());
    {
        let mut inner = state.lock_inner();
        set_tempo(&mut inner, 120.0);
        inner.sync_history();
    }
    assert_eq!(Journal::open(&path).unwrap().len(), 1);
    // New writes the file at the path first; the open document is the same.
    std::fs::remove_file(&path).unwrap();
    crate::commands::create_project_in(&mut state.lock_inner(), Some(path.clone())).unwrap();
    assert_eq!(
        Journal::open(&path).unwrap().len(),
        0,
        "no outgoing entry carried into the new history"
    );
    assert_eq!(state.lock_inner().undo.total_levels(), 0);
}

#[test]
fn a_failed_new_leaves_the_open_document_and_its_history_alone() {
    super::enable_persistence_for_test();
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let path = project_file(dir.path());
    {
        let mut inner = state.lock_inner();
        set_tempo(&mut inner, 120.0);
        inner.sync_history();
    }
    let target = dir.path().join("missing").join("New.ocp");
    {
        let mut inner = state.lock_inner();
        let before = inner.project.clone();
        assert!(crate::commands::create_project_in(&mut inner, Some(target)).is_err());
        assert_eq!(inner.project, before, "the outgoing document stayed");
        assert_eq!(inner.file_path.as_deref(), Some(path.as_path()));
        assert_eq!(inner.undo.total_levels(), 1);
        assert!(inner.dirty, "the unsaved edit stayed unsaved");
    }
    assert_eq!(Journal::open(&path).unwrap().len(), 1);
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

#[test]
fn start_fresh_replaces_a_history_with_an_unfingerprinted_marker() {
    let dir = tempfile::tempdir().unwrap();
    super::enable_persistence_for_test();
    let path = dir.path().join("fresh.ocp");
    std::fs::write(&path, SOURCE).unwrap();

    // An old history with an entry and a fingerprint stands in for another file.
    let mut journal = Journal::open(&path).unwrap();
    journal.append(&[entry(1)]).unwrap();
    journal::write_marker(
        journal.dir(),
        &journal::Marker::new(&path, Some("old-fingerprint".into())),
    )
    .unwrap();
    drop(journal);

    super::start_fresh(&path);

    let marker = journal::read_marker(&journal::dir_for(&path)).unwrap();
    assert_eq!(marker.path, crate::project_data::canonical_path(&path));
    assert_eq!(marker.sha256, None);
    assert!(
        !journal::dir_for(&path).join("entries.jsonl").exists(),
        "the old entries survived"
    );
}

#[test]
fn start_fresh_keeps_attach_from_adopting_a_matching_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    super::enable_persistence_for_test();
    let first = dir.path().join("first.ocp");
    std::fs::write(&first, SOURCE).unwrap();
    let state = AppState::new();
    crate::commands::open_project(&mut state.lock_inner(), SOURCE, Some(first.clone())).unwrap();
    set_tempo(&mut state.lock_inner(), 130.0);
    crate::commands::save_file_inner(None, &state).unwrap();
    drop(state);

    // The same bytes at another path would match the first history by content.
    let bytes = std::fs::read(&first).unwrap();
    let second = dir.path().join("second.ocp");
    super::start_fresh(&second);
    std::fs::write(&second, &bytes).unwrap();

    let reopened = reopen(&second);
    assert_eq!(
        reopened.lock_inner().undo.total_levels(),
        0,
        "the new file adopted another file's history"
    );
    assert!(
        journal::dir_for(&first).exists(),
        "the first history moved to the new file"
    );
}

#[test]
fn a_history_page_entry_carries_typed_changes_and_no_display_text() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let mut inner = state.lock_inner();
    set_tempo(&mut inner, 132.0);
    inner.sync_history();
    let page = inner
        .undo
        .history_page(None, 64, &inner.project.properties)
        .unwrap();
    let entry = &page.entries[0];
    assert_eq!(
        entry.changes.changes,
        vec![Change::Tempo {
            from: 100.0,
            to: 132.0
        }]
    );
    let wire = serde_json::to_value(entry).unwrap();
    assert!(wire.get("name").is_none(), "{wire}");
    assert!(wire.get("subject").is_none(), "{wire}");
    assert_eq!(
        wire["changes"]["changes"],
        serde_json::json!([{"kind": "tempo", "from": 100.0, "to": 132.0}])
    );
}

#[test]
fn a_merged_gesture_describes_its_whole_run() {
    let mut stack = crate::state::UndoStack::new();
    let state = AppState::new();
    let mut project = state.inner.into_inner().unwrap().project;
    let start = project.properties.tempo;
    stack.begin_gesture(UndoScope::Transport);
    for (step, tempo) in [110.0, 125.0, 140.0].into_iter().enumerate() {
        let old = project.clone();
        project.properties.tempo = tempo;
        if step == 2 {
            project.name = Some("Final".into());
        }
        stack
            .push(old, &project, UndoEdit::new("tempo", UndoScope::Transport))
            .unwrap();
    }
    stack.end_gesture();
    let history = stack.history(&project.properties);
    assert_eq!(history.len(), 1);
    assert_eq!(
        history[0].changes.changes,
        vec![
            Change::ProjectRenamed {
                from: Some("Untitled".into()),
                to: Some("Final".into()),
            },
            Change::Tempo {
                from: start,
                to: 140.0
            },
        ]
    );
}

#[test]
fn a_conversation_revert_cause_survives_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    {
        let mut inner = state.lock_inner();
        let mut target = inner.project.clone();
        target.properties.tempo = 140.0;
        crate::commands::restore_dsl_with_undo(
            &mut inner,
            oc_dsl::serialize(&target),
            "revert conversation",
            EditCause::ConversationRevert,
        )
        .unwrap();
        inner.sync_history();
    }
    crate::commands::save_file_inner(None, &state).unwrap();
    drop(state);

    let path = project_file(dir.path());
    let source = std::fs::read_to_string(&path).unwrap();
    let reopened = AppState::new();
    let mut inner = reopened.lock_inner();
    crate::commands::open_project(&mut inner, &source, Some(path)).unwrap();
    let page = inner
        .undo
        .history_page(None, 64, &inner.project.properties)
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].cause, EditCause::ConversationRevert);
    assert_eq!(
        page.entries[0].changes.changes,
        vec![Change::Tempo {
            from: 100.0,
            to: 140.0
        }]
    );
}

#[test]
fn a_journal_line_with_old_display_keys_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(dir.path());
    let stored = entry(7);
    let mut line = serde_json::to_value(&stored).unwrap();
    line["label"] = serde_json::json!("tempo");
    line["subject"] = serde_json::json!("128 BPM");
    let mut text = serde_json::to_string(&line).unwrap();
    text.push('\n');
    let history = journal::dir_for(&path);
    std::fs::create_dir_all(&history).unwrap();
    std::fs::write(history.join("entries.jsonl"), &text).unwrap();
    let mut row = Vec::new();
    row.extend_from_slice(&stored.id.to_le_bytes());
    row.extend_from_slice(&0u64.to_le_bytes());
    row.extend_from_slice(&(text.len() as u64).to_le_bytes());
    std::fs::write(history.join("index.bin"), &row).unwrap();

    let journal = Journal::open(&path).unwrap();
    let loaded = journal.read_range(0..1).unwrap().remove(0);
    assert_eq!(loaded, stored);
}

fn tempo(tempo: u32) -> oc_dsl::ast::Project {
    oc_dsl::parse(&format!("@project \"Song\"\n  tempo: {tempo}\n")).unwrap()
}

fn push_edit(
    stack: &mut UndoStack,
    before: &oc_dsl::ast::Project,
    after: &oc_dsl::ast::Project,
    author: Author,
) {
    stack
        .push(
            before.clone(),
            after,
            UndoEdit::new("test edit", UndoScope::Transport).by(author),
        )
        .unwrap();
}

fn stack(journaled: bool, dir: &Path) -> UndoStack {
    let mut stack = UndoStack::new();
    if journaled {
        let journal = Journal::open(&project_file(dir)).unwrap();
        stack.attach_journal(journal).unwrap();
    }
    stack
}

#[test]
fn a_person_edit_after_a_mark_is_listed_with_its_changes() {
    for journaled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut stack = stack(journaled, dir.path());
        let mut project = tempo(100);
        let mark = stack.mark();
        assert_eq!(mark.at, None);
        let before = project.clone();
        project = tempo(120);
        push_edit(&mut stack, &before, &project, Author::Person);
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
        }
        let changes = stack
            .since(&mark, "s1", &project.properties)
            .expect("the person's edit is news");
        assert_eq!(changes.entries.len(), 1);
        assert_eq!(changes.more, 0);
        assert_eq!(changes.undone, 0);
        assert!(!changes.rewritten);
        assert_eq!(changes.entries[0].author, Author::Person);
        assert_eq!(changes.entries[0].cause, EditCause::Edit);
        assert_eq!(
            changes.entries[0].changes.changes,
            vec![Change::Tempo {
                from: 100.0,
                to: 120.0,
            }]
        );
    }
}

#[test]
fn a_sessions_own_edit_is_not_listed_but_another_sessions_is() {
    for journaled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut stack = stack(journaled, dir.path());
        let mut project = tempo(100);
        let mark = stack.mark();
        let before = project.clone();
        project = tempo(110);
        push_edit(
            &mut stack,
            &before,
            &project,
            Author::Agent {
                session: Some("s1".into()),
            },
        );
        assert!(
            stack.since(&mark, "s1", &project.properties).is_none(),
            "a conversation does not report its own edits"
        );
        let before = project.clone();
        project = tempo(120);
        push_edit(
            &mut stack,
            &before,
            &project,
            Author::Agent {
                session: Some("s2".into()),
            },
        );
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
        }
        let changes = stack
            .since(&mark, "s1", &project.properties)
            .expect("the other conversation's edit is news");
        assert_eq!(changes.entries.len(), 1);
        assert_eq!(
            changes.entries[0].author,
            Author::Agent {
                session: Some("s2".into())
            }
        );
        assert_eq!(
            changes.entries[0].changes.changes,
            vec![Change::Tempo {
                from: 110.0,
                to: 120.0,
            }]
        );
    }
}

#[test]
fn entry_ids_stay_within_the_javascript_number_range() {
    let mut stack = UndoStack::new();
    let mut project = tempo(100);
    for step in 0..4 {
        let before = project.clone();
        project = tempo(110 + step);
        push_edit(&mut stack, &before, &project, Author::Person);
        let id = stack.undo.last().unwrap().id;
        assert!(id <= crate::state::MAX_ENTRY_ID, "{id}");
    }
}

#[test]
fn a_new_history_does_not_match_an_old_recall_position() {
    let dir = tempfile::tempdir().unwrap();
    let state = opened(dir.path(), SOURCE);
    let path = project_file(dir.path());
    let at = {
        let mut inner = state.lock_inner();
        set_tempo(&mut inner, 120.0);
        inner.sync_history();
        crate::ai::sessions::HistoryAt::of(&inner)
            .unwrap()
            .at
            .unwrap()
    };
    // New replaces the history for the same path; the saved position must not
    // find an unrelated entry in it.
    std::fs::remove_file(&path).unwrap();
    {
        let mut inner = state.lock_inner();
        crate::commands::create_project_in(&mut inner, Some(path)).unwrap();
        set_tempo(&mut inner, 130.0);
        assert_eq!(inner.undo.level_of(at), None);
    }
}

#[test]
fn level_of_finds_a_mark_on_either_side_of_the_cursor() {
    for journaled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut stack = stack(journaled, dir.path());
        let mut project = tempo(100);
        let mut ids = Vec::new();
        for tempo_value in [110, 120, 130] {
            let before = project.clone();
            project = tempo(tempo_value);
            push_edit(&mut stack, &before, &project, Author::Person);
            ids.push(stack.undo.last().unwrap().id);
            if journaled {
                stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
            }
        }
        assert_eq!(stack.level_of(ids[0]), Some(1));
        assert_eq!(stack.level_of(ids[1]), Some(2));
        assert_eq!(stack.level_of(ids[2]), Some(3));
        stack.undo(&mut project).unwrap();
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
        }
        assert_eq!(
            stack.level_of(ids[1]),
            Some(2),
            "a redo entry keeps its level"
        );
        assert_eq!(stack.level_of(999_999), None);
    }
}

#[test]
fn entries_between_reads_only_the_requested_applied_levels() {
    for journaled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut stack = stack(journaled, dir.path());
        let mut project = tempo(100);
        for (tempo_value, author) in [
            (110, Author::Person),
            (
                120,
                Author::Agent {
                    session: Some("s2".into()),
                },
            ),
            (
                130,
                Author::Agent {
                    session: Some("s1".into()),
                },
            ),
        ] {
            let before = project.clone();
            project = tempo(tempo_value);
            push_edit(&mut stack, &before, &project, author);
        }
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
        }
        let entries = stack.entries_between(1, 3, &project.properties).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].0,
            Author::Agent {
                session: Some("s2".into())
            }
        );
        assert_eq!(
            entries[0].2.changes,
            vec![Change::Tempo {
                from: 110.0,
                to: 120.0,
            }]
        );
        assert_eq!(
            entries[1].0,
            Author::Agent {
                session: Some("s1".into())
            }
        );
        assert_eq!(
            entries[1].2.changes,
            vec![Change::Tempo {
                from: 120.0,
                to: 130.0,
            }]
        );
        assert!(stack.entries_between(2, 1, &project.properties).is_err());
        assert!(stack.entries_between(0, 4, &project.properties).is_err());
        stack.undo(&mut project).unwrap();
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
        }
        assert!(
            stack.entries_between(0, 3, &project.properties).is_err(),
            "redo levels are not applied entries"
        );
    }
}

#[test]
fn an_undo_past_the_mark_reports_the_steps_without_entries() {
    for journaled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut stack = stack(journaled, dir.path());
        let mut project = tempo(100);
        let before = project.clone();
        project = tempo(110);
        push_edit(&mut stack, &before, &project, Author::Person);
        let before = project.clone();
        project = tempo(120);
        push_edit(
            &mut stack,
            &before,
            &project,
            Author::Agent {
                session: Some("s1".into()),
            },
        );
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
        }
        let mark = stack.mark();
        stack.undo(&mut project).unwrap();
        let changes = stack
            .since(&mark, "s1", &project.properties)
            .expect("undo moved the project back");
        assert!(changes.entries.is_empty());
        assert_eq!(changes.undone, 1);
        assert!(!changes.rewritten);
        assert_eq!(changes.more, 0);
    }
}

#[test]
fn an_undo_past_the_mark_and_a_new_edit_reads_as_rewritten() {
    for journaled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut stack = stack(journaled, dir.path());
        let mut project = tempo(100);
        for tempo_value in [110, 120, 130] {
            let before = project.clone();
            project = tempo(tempo_value);
            push_edit(&mut stack, &before, &project, Author::Person);
        }
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
        }
        let mark = stack.mark();
        stack.undo(&mut project).unwrap();
        stack.undo(&mut project).unwrap();
        let before = project.clone();
        project = tempo(140);
        push_edit(&mut stack, &before, &project, Author::Person);
        let changes = stack
            .since(&mark, "s1", &project.properties)
            .expect("a rewritten history is news");
        assert!(changes.rewritten);
        assert_eq!(changes.undone, 0);
        assert_eq!(changes.entries.len(), 1);
        assert_eq!(changes.more, 0);
        assert_eq!(
            changes.entries[0].changes.changes,
            vec![Change::Tempo {
                from: 110.0,
                to: 140.0,
            }]
        );
    }
}

#[test]
fn since_reports_own_edits_no_longer_applied() {
    let agent = Author::Agent {
        session: Some("s1".into()),
    };
    for journaled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut stack = stack(journaled, dir.path());
        let mut project = tempo(100);
        let mark = stack.mark();
        let before = project.clone();
        project = tempo(110);
        push_edit(&mut stack, &before, &project, agent.clone());
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
        }
        stack.undo(&mut project).unwrap();
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
        }
        let changes = stack
            .since(&mark, "s1", &project.properties)
            .expect("an undone own edit is news");
        assert_eq!(changes.own_undone, 1);
        assert!(changes.entries.is_empty());

        stack.redo(&mut project).unwrap();
        stack.undo(&mut project).unwrap();
        let before = project.clone();
        project = tempo(130);
        push_edit(&mut stack, &before, &project, Author::Person);
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
        }
        let changes = stack
            .since(&mark, "s1", &project.properties)
            .expect("a discarded own edit is news");
        assert_eq!(changes.own_undone, 1, "the redo branch took the own edit");
        assert_eq!(changes.entries.len(), 1);
        assert_eq!(changes.entries[0].author, Author::Person);
    }
}

#[test]
fn a_pruned_mark_reports_pruning_not_a_rewrite() {
    for journaled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut stack = stack(journaled, dir.path());
        let mut project = tempo(100);
        for tempo_value in [110, 120, 130] {
            let before = project.clone();
            project = tempo(tempo_value);
            push_edit(&mut stack, &before, &project, Author::Person);
            if journaled {
                stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
            }
        }
        let mark = stack.mark();
        for tempo_value in [140, 150] {
            let before = project.clone();
            project = tempo(tempo_value);
            push_edit(&mut stack, &before, &project, Author::Person);
            if journaled {
                stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
            }
        }
        assert!(stack.clear_older(2).unwrap() > 0);
        let changes = stack
            .since(&mark, "s1", &project.properties)
            .expect("new entries and the dropped mark are news");
        assert!(changes.pruned, "cleanup dropped the mark");
        assert!(!changes.rewritten, "nothing replaced the earlier history");
        let newest: Vec<u32> = changes
            .entries
            .iter()
            .filter_map(|entry| match entry.changes.changes.first() {
                Some(Change::Tempo { to, .. }) => Some(*to as u32),
                _ => None,
            })
            .collect();
        assert_eq!(newest, [150, 140]);
    }
}

#[test]
fn ten_outside_edits_list_the_newest_eight() {
    for journaled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut stack = stack(journaled, dir.path());
        let mut project = tempo(100);
        let mark = stack.mark();
        for tempo_value in 101..=110 {
            let before = project.clone();
            project = tempo(tempo_value);
            push_edit(&mut stack, &before, &project, Author::Person);
        }
        if journaled {
            stack.sync_journal(&oc_dsl::serialize(&project)).unwrap();
        }
        let changes = stack
            .since(&mark, "s1", &project.properties)
            .expect("the edits are news");
        assert_eq!(changes.entries.len(), MAX_OUTSIDE_ENTRIES);
        assert_eq!(changes.more, 2);
        let newest: Vec<u32> = changes
            .entries
            .iter()
            .filter_map(|entry| match entry.changes.changes.first() {
                Some(Change::Tempo { to, .. }) => Some(*to as u32),
                _ => None,
            })
            .collect();
        assert_eq!(newest, [110, 109, 108, 107, 106, 105, 104, 103]);
    }
}

#[test]
fn an_unchanged_history_has_no_outside_changes() {
    for journaled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let stack = stack(journaled, dir.path());
        let project = tempo(100);
        let mark = stack.mark();
        assert!(stack.since(&mark, "s1", &project.properties).is_none());
    }
}
