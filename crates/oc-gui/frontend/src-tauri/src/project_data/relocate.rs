//! One routine that gives everything the app keeps for a project to another
//! path, including the edit history, the AI note, the conversations and the
//! recovery copies. Callers run it without the document lock; it takes the
//! destination's process lock and each store keeps its own lock.

use std::path::{Path, PathBuf};

use crate::edit_history::journal;

/// How the data follows the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relocation {
    /// The file moved: Move Project, or a file moved outside the app. The data
    /// leaves the old path.
    Move,
    /// The file was copied: Save As, or a file copied outside the app. The
    /// old path keeps its data; conversations and recovery copies stay with
    /// it, the note and the history are copied.
    Copy,
}

/// What one store did during a relocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    /// The old path kept no data for this store.
    Absent,
    Moved,
    Copied,
    /// The store failed; its data stays at the old path.
    Failed,
}

impl StoreOutcome {
    pub(crate) fn is_failure(self) -> bool {
        self == Self::Failed
    }
}

/// What every store did, so the caller can name the history's new home and
/// report a store that stayed behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelocationReport {
    pub history: StoreOutcome,
    pub note: StoreOutcome,
    pub conversations: StoreOutcome,
    pub recovery: StoreOutcome,
}

/// A relocation captured under the document lock, to run after it drops.
#[derive(Debug, Clone)]
pub(crate) struct PendingRelocation {
    pub(crate) from: PathBuf,
    pub(crate) to: PathBuf,
    pub(crate) how: Relocation,
}

impl PendingRelocation {
    pub(crate) fn run(self) -> RelocationReport {
        relocate(&self.from, &self.to, self.how)
    }
}

/// Give every store of `from` to `to`. Each store moves on its own; one that
/// fails is logged and keeps its data at the old path, which the stale sweep
/// reports instead of losing. The history is a store like the rest.
pub fn relocate(from: &Path, to: &Path, how: Relocation) -> RelocationReport {
    // The destination belongs to the project being opened; another process
    // holding it keeps the data where it is instead of mixing the two.
    let _to_lock =
        match crate::process_lock::ProcessLock::acquire(crate::project_data::project_lock_path(to))
        {
            Ok(lock) => lock,
            Err(error) => {
                log::warn!(
                    "[project data] {} stays at {}: {error}",
                    to.display(),
                    from.display()
                );
                return RelocationReport {
                    history: StoreOutcome::Failed,
                    note: StoreOutcome::Failed,
                    conversations: StoreOutcome::Failed,
                    recovery: StoreOutcome::Failed,
                };
            }
        };
    let copy = how == Relocation::Copy;
    let history = relocate_history(from, to, how);
    let note = match crate::ai::project_instructions::Store::config().relocate(from, to, copy) {
        Ok(_) if copy => StoreOutcome::Copied,
        Ok(_) => StoreOutcome::Moved,
        Err(error) => {
            log::warn!(
                "[project data] the AI note of {} stays: {error}",
                from.display()
            );
            StoreOutcome::Failed
        }
    };
    if copy {
        return RelocationReport {
            history,
            note,
            conversations: StoreOutcome::Absent,
            recovery: StoreOutcome::Absent,
        };
    }
    let conversations = match crate::ai::sessions::Store::config().relocate_project(from, to) {
        Ok(count) => {
            if count > 0 {
                log::info!(
                    "[project data] {count} conversation(s) follow {}",
                    to.display()
                );
            }
            StoreOutcome::Moved
        }
        Err(error) => {
            log::warn!(
                "[project data] conversations of {} stay: {error}",
                from.display()
            );
            StoreOutcome::Failed
        }
    };
    crate::autosave::relocate_copies(&crate::autosave::app_root(), from, to);
    RelocationReport {
        history,
        note,
        conversations,
        recovery: StoreOutcome::Moved,
    }
}

/// Move or copy the history directory of `from` to the one for `to`.
fn relocate_history(from: &Path, to: &Path, how: Relocation) -> StoreOutcome {
    let source = journal::dir_for(from);
    if !source.is_dir() {
        return StoreOutcome::Absent;
    }
    let destination = journal::dir_for(to);
    let result = match how {
        Relocation::Copy => journal::copy_dir(&source, &destination),
        Relocation::Move => journal::move_dir(&source, &destination),
    };
    match result {
        Ok(()) if how == Relocation::Copy => StoreOutcome::Copied,
        Ok(()) => StoreOutcome::Moved,
        Err(error) => {
            log::warn!("[history] the history of {} stays: {error}", from.display());
            StoreOutcome::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ai::{project_instructions, sessions};
    use crate::state::AppState;

    #[test]
    fn move_project_takes_the_note_and_the_conversations_along() {
        let dir = tempfile::tempdir().unwrap();
        let _sessions = sessions::test_root(dir.path().join("sessions"));
        let _notes = project_instructions::test_root(&dir.path().join("notes"));
        let folder = dir.path().join("songs");
        std::fs::create_dir(&folder).unwrap();
        let from = folder.join("song.ocp");
        let source = "@project \"Song\"\n  tempo: 100\n";
        std::fs::write(&from, source).unwrap();
        let state = AppState::new();
        crate::commands::open_project(&mut state.lock_inner(), source, Some(from.clone())).unwrap();
        project_instructions::Store::config()
            .save(&from, "Keep the bass dry.")
            .unwrap();
        let store = sessions::Store::config();
        let conversation = store
            .create(Some(&folder), Some(&from), Some("Bass"), None)
            .unwrap();
        // A conversation in a chosen workspace above the project stays there.
        let scoped = store
            .create(Some(dir.path()), Some(&from), Some("Scoped"), None)
            .unwrap();

        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let to = other.join("moved.ocp");
        crate::commands::move_project_inner(&state, to.clone()).unwrap();

        let notes = project_instructions::Store::config();
        assert_eq!(notes.load(&to).unwrap().text, "Keep the bass dry.");
        assert!(
            notes
                .notes()
                .iter()
                .all(|note| note.key != super::super::path_key(&from)),
            "the note left the old key"
        );
        let canonical = super::super::canonical_path(&to)
            .to_string_lossy()
            .to_string();
        let (_, moved) = store.find(&conversation.id).unwrap();
        assert_eq!(moved.project_path.as_deref(), Some(canonical.as_str()));
        assert_eq!(moved.workspace_key, super::super::path_key(&other));
        let (_, stayed) = store.find(&scoped.id).unwrap();
        assert_eq!(stayed.project_path.as_deref(), Some(canonical.as_str()));
        assert_eq!(stayed.workspace_key, scoped.workspace_key);
    }

    #[test]
    fn save_as_copies_the_note_and_leaves_the_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let _sessions = sessions::test_root(dir.path().join("sessions"));
        let _notes = project_instructions::test_root(&dir.path().join("notes"));
        let from = dir.path().join("song.ocp");
        let source = "@project \"Song\"\n  tempo: 100\n";
        std::fs::write(&from, source).unwrap();
        let state = AppState::new();
        crate::commands::open_project(&mut state.lock_inner(), source, Some(from.clone())).unwrap();
        project_instructions::Store::config()
            .save(&from, "Note")
            .unwrap();
        let conversation = sessions::Store::config()
            .create(Some(dir.path()), Some(&from), Some("Bass"), None)
            .unwrap();
        let copy = dir.path().join("copy.ocp");
        crate::commands::save_file_inner(Some(copy.display().to_string()), &state).unwrap();
        let notes = project_instructions::Store::config();
        assert_eq!(notes.load(&copy).unwrap().text, "Note");
        assert_eq!(notes.load(&from).unwrap().text, "Note");
        let (_, kept) = sessions::Store::config().find(&conversation.id).unwrap();
        assert_eq!(kept.project_path, conversation.project_path);
    }
}
