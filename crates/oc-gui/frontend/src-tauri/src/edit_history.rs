//! The project's edit history: who made each undoable edit, and the journal
//! that keeps the history in the app's data folder so it outlives the process.
//!
//! The in-memory [`crate::state::UndoStack`] holds a window of entries around
//! the cursor; [`journal::Journal`] holds all of them. The stack writes its
//! changes to the journal at settle points (a committed edit, an undo, a tick)
//! and reloads its window from it when the cursor leaves the window.

use std::cell::{Cell, RefCell};

use serde::{Deserialize, Serialize};

pub mod journal;
#[cfg(test)]
mod tests;

/// Who made an edit. Persisted in the journal, so variants and their fields
/// are wire names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Author {
    /// The person at the GUI.
    #[default]
    Person,
    /// The agent of a conversation; `session` is absent for a panel turn that
    /// has no saved conversation yet.
    Agent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// An external `/call` or `/mcp` client not bound to a conversation.
    Client { owner: String },
    /// The file changed while OpenCompose was not looking at it; the entry
    /// bridges the history's last document to the file's.
    Outside,
}

impl Author {
    /// The author a caller identity (a batch owner) stands for.
    pub fn from_caller(caller: &str) -> Self {
        if let Some(session) = crate::state::caller_conversation(caller) {
            return Author::Agent {
                session: Some(session.to_owned()),
            };
        }
        if crate::state::loop_batch_owner(caller) {
            return Author::Agent { session: None };
        }
        if crate::state::is_external_caller(caller) {
            return Author::Client {
                owner: caller.to_owned(),
            };
        }
        Author::Person
    }
}

thread_local! {
    static ACTING: RefCell<Option<Author>> = const { RefCell::new(None) };
    static PERSIST_IN_TEST: Cell<bool> = const { Cell::new(false) };
}

/// The author edits on this thread are recorded for; the person by default.
pub fn acting() -> Author {
    ACTING.with(|acting| acting.borrow().clone().unwrap_or_default())
}

/// Restores the previous acting author when dropped.
pub struct Acting(Option<Author>);

impl Drop for Acting {
    fn drop(&mut self) {
        let previous = self.0.take();
        ACTING.with(|acting| *acting.borrow_mut() = previous);
    }
}

/// Record edits made on this thread, until the guard drops, as `author`'s.
/// A tool call runs its edit on the calling thread; work it moves to another
/// thread is recorded as the person's.
pub fn act_as(author: Author) -> Acting {
    Acting(ACTING.with(|acting| acting.borrow_mut().replace(author)))
}

/// Whether histories are written to disk. A test build writes none unless the
/// test asks, and then into a folder of the test thread's own.
pub fn persistence_enabled() -> bool {
    !cfg!(test) || PERSIST_IN_TEST.with(Cell::get)
}

/// Let this test thread write histories for the projects it opens.
#[cfg(test)]
pub fn enable_persistence_for_test() {
    PERSIST_IN_TEST.with(|enabled| enabled.set(true));
}

/// Remove the history of `project`.
pub fn discard(project: &std::path::Path) {
    if !persistence_enabled() {
        return;
    }
    let dir = journal::dir_for(project);
    if let Err(error) = std::fs::remove_dir_all(&dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("[history] cannot remove {}: {error}", dir.display());
    }
}

/// Start an empty history for a new document written at `project`. The
/// history exists before the file is opened, so the open never adopts another
/// project's history by fingerprint: a new file can match an old one's bytes.
pub fn start_fresh(project: &std::path::Path) {
    discard(project);
    if !persistence_enabled() {
        return;
    }
    let dir = journal::dir_for(project);
    if let Err(error) = journal::write_marker(&dir, &journal::Marker::new(project, None)) {
        log::warn!("[history] cannot start {}: {error}", dir.display());
    }
}

/// What the footer and the History tab show for one entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Brief {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub author: Author,
}
