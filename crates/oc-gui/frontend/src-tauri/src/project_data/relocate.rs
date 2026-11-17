//! One routine that gives everything the app keeps for a project to another
//! path. The edit history moves with the open document (see
//! `AppStateInner::rehome_history`); this moves the rest.

use std::path::Path;

/// How the data follows the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relocation {
    /// The file moved: Move Project, or a file moved outside the app. The data
    /// leaves the old path.
    Move,
    /// The file was copied: Save As, or a file copied outside the app. The
    /// old path keeps its data; conversations and recovery copies stay with
    /// it, the note is copied.
    Copy,
}

/// Give the AI note, the conversations and the recovery copies of `from` to
/// `to`. Each store is moved on its own; one that fails is logged and keeps
/// its data at the old path, which the stale sweep reports instead of losing.
pub fn relocate(from: &Path, to: &Path, how: Relocation) {
    if !crate::edit_history::persistence_enabled() {
        return;
    }
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
                return;
            }
        };
    let copy = how == Relocation::Copy;
    if let Err(error) = crate::ai::project_instructions::Store::config().relocate(from, to, copy) {
        log::warn!(
            "[project data] the AI note of {} stays: {error}",
            from.display()
        );
    }
    if copy {
        return;
    }
    match crate::ai::sessions::Store::config().relocate_project(from, to) {
        Ok(count) if count > 0 => {
            log::info!(
                "[project data] {count} conversation(s) follow {}",
                to.display()
            );
        }
        Ok(_) => {}
        Err(error) => {
            log::warn!(
                "[project data] conversations of {} stay: {error}",
                from.display()
            );
        }
    }
    crate::autosave::relocate_copies(&crate::autosave::app_root(), from, to);
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
        crate::edit_history::enable_persistence_for_test();
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
        crate::edit_history::enable_persistence_for_test();
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
