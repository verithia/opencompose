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

/// Record the content SHA-256 of `project` after the app wrote it, so its
/// history is found again if the file moves outside the app. The caller holds
/// the document lock, so a marker write cannot race a history rewrite.
pub fn record_fingerprint(project: &std::path::Path, sha256: Option<String>) {
    if !persistence_enabled() || sha256.is_none() {
        return;
    }
    let dir = journal::dir_for(project);
    if let Err(error) = journal::write_marker(&dir, &journal::Marker::new(project, sha256)) {
        log::warn!("[history] cannot record {}: {error}", project.display());
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

/// What a patch cannot say about why an entry exists.
///
/// A conversation restore writes a saved document, so its diff names the same
/// facts as an ordinary edit. The persisted cause lets a UI describe an entry
/// written by that flow as a conversation revert instead. Recall moves the
/// cursor and never writes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditCause {
    /// An ordinary document edit.
    #[default]
    Edit,
    /// A conversation restored an earlier document snapshot.
    ConversationRevert,
}

impl EditCause {
    /// Whether this is the default cause, which a stored entry omits.
    pub fn is_edit(&self) -> bool {
        matches!(self, Self::Edit)
    }
}

/// What the footer and the History tab show for one entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Brief {
    pub changes: oc_dsl::change::ChangeSet,
    pub cause: EditCause,
    pub author: Author,
}

/// Entries one outside-changes notice describes.
pub const MAX_OUTSIDE_ENTRIES: usize = 8;

/// Largest count an outside-changes notice may report.
pub const MAX_OUTSIDE_COUNT: usize = 1_000_000;

/// Longest author identity an outside-changes notice may carry.
pub const MAX_OUTSIDE_AUTHOR_CHARS: usize = 512;

/// A cursor position in a project's edit history. `at` is the entry the cursor
/// rests on after its newest applied edit; `max_id` is the largest id the
/// history holds, so a reader can tell a rewritten history from an extended one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryMark {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    pub max_id: u64,
}

/// One edit another author made while a conversation was away.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutsideEntry {
    pub author: Author,
    pub cause: EditCause,
    pub changes: oc_dsl::change::ChangeSet,
}

/// What changed in a project outside one conversation, and how far undo moved
/// the cursor back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutsideChanges {
    /// Newest first, bounded by [`MAX_OUTSIDE_ENTRIES`].
    pub entries: Vec<OutsideEntry>,
    /// Listed edits beyond the described ones.
    pub more: usize,
    /// Steps undo took back past the conversation's mark.
    pub undone: usize,
    /// This conversation's own edits since the mark that a person undid or
    /// replaced, so the model does not assume they are still applied.
    pub own_undone: usize,
    /// Whether the entry at the mark is gone, so earlier entries may differ.
    pub rewritten: bool,
    /// Whether history cleanup dropped the entry at the mark. The earlier
    /// entries are simply no longer kept; they were not replaced.
    pub pruned: bool,
}
