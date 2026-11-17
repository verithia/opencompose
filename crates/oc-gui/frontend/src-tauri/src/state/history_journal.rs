//! The undo stack's window over the project's journal in the app data folder.
//!
//! Every in-memory change is written at a settle point by [`UndoStack::sync_journal`],
//! which compares the window with the journal entry by entry, so a squash, a
//! rollback, a merge or a put-back needs no journal code of its own.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use oc_dsl::ast::{Project, ProjectProperties};
use oc_dsl::diff::{self, compressed};

use super::{
    AppStateInner, HISTORY_PAGE, MAX_UNDO, NEXT_ENTRY_ID, UndoEdit, UndoEntry, UndoHistoryEntry,
    UndoScope, UndoStack, entry_changes,
};
use crate::edit_history::journal::{self, Journal, StoredEntry};
use crate::edit_history::{
    Author, Brief, EditCause, HistoryMark, MAX_OUTSIDE_COUNT, MAX_OUTSIDE_ENTRIES, OutsideChanges,
    OutsideEntry,
};
use crate::project_data::{Relocation, RelocationReport};

/// Entries a window reload keeps below the cursor; the rest stay on disk.
const WINDOW_BELOW: usize = 2 * HISTORY_PAGE;

/// Why a history operation was refused.
#[derive(Debug)]
pub enum HistoryError {
    /// The in-memory window holds entries the journal has not received yet.
    NotSynced,
    /// A batch holds provisional entries, so the window may not move.
    InBatch,
    /// The document has no journal.
    NotJournaled,
    /// Reading or writing the journal failed.
    Io(std::io::Error),
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSynced => formatter.write_str("the edit history is not saved yet; try again"),
            Self::InBatch => {
                formatter.write_str("older history becomes available once the agent's batch ends")
            }
            Self::NotJournaled => formatter.write_str("the edit history is not kept on disk"),
            Self::Io(error) => {
                write!(
                    formatter,
                    "the project's edit history could not be used: {error}"
                )
            }
        }
    }
}

impl std::error::Error for HistoryError {}

impl From<std::io::Error> for HistoryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<HistoryError> for String {
    fn from(error: HistoryError) -> Self {
        error.to_string()
    }
}

/// Whether an adopted journal already holds the window (`Copy`) or is empty
/// (`Fresh`). The window keeps its base and redo depth only for a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JournalHome {
    Fresh,
    Copy,
}

impl RelocationReport {
    /// The history home a relocation left for the open document.
    pub(crate) fn history_home(&self) -> JournalHome {
        match self.history {
            crate::project_data::StoreOutcome::Copied
            | crate::project_data::StoreOutcome::Moved => JournalHome::Copy,
            _ => JournalHome::Fresh,
        }
    }
}

/// One page of the history, newest first, for the History tab.
#[derive(Debug, Clone, Serialize)]
pub struct HistoryPage {
    pub entries: Vec<UndoHistoryEntry>,
    /// Entries in the whole history, applied or not.
    pub total: usize,
    /// How many of them, from the oldest, are applied.
    pub applied: usize,
    /// Whether the history is kept on disk for the project file.
    pub journaled: bool,
}

/// Where the history lives and what it occupies.
#[derive(Debug, Clone, Serialize)]
pub struct HistoryInfo {
    pub total: usize,
    pub applied: usize,
    pub journaled: bool,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The stored form: patches in their forward orientation, whichever side of
/// the cursor the entry is on.
fn stored(entry: &UndoEntry, redo_side: bool) -> StoredEntry {
    let (forward, inverse) = if redo_side {
        (entry.inverse.clone(), entry.forward.clone())
    } else {
        (entry.forward.clone(), entry.inverse.clone())
    };
    StoredEntry {
        id: entry.id,
        at_ms: millis(entry.timestamp),
        cause: entry.cause,
        author: entry.author.clone(),
        scopes: entry.scopes,
        forward,
        inverse,
    }
}

fn from_stored(entry: StoredEntry, redo_side: bool) -> UndoEntry {
    let (forward, inverse) = if redo_side {
        (entry.inverse, entry.forward)
    } else {
        (entry.forward, entry.inverse)
    };
    UndoEntry {
        id: entry.id,
        author: entry.author,
        cause: entry.cause,
        forward,
        inverse,
        // A stored entry carries no display text; the tag only feeds logs.
        tag: "history entry".into(),
        scopes: entry.scopes,
        timestamp: UNIX_EPOCH + Duration::from_millis(entry.at_ms),
    }
}

fn page_entry(
    position: usize,
    applied: usize,
    entry: StoredEntry,
    properties: &ProjectProperties,
) -> UndoHistoryEntry {
    UndoHistoryEntry {
        index: position + 1,
        changes: entry_changes(&entry.forward, &entry.inverse, properties),
        cause: entry.cause,
        author: entry.author,
        scopes: entry.scopes,
        timestamp: entry.at_ms,
        applied: position < applied,
    }
}

fn io_text(error: std::io::Error) -> HistoryError {
    HistoryError::Io(error)
}

fn restore_text(reason: String) -> HistoryError {
    HistoryError::Io(std::io::Error::other(reason))
}

/// Where one zero-based history position lives.
enum Slot {
    /// An entry older than the window, read from the journal.
    Below(usize),
    /// An applied entry in the window.
    Undo(usize),
    /// An undone entry in the window, newest first.
    Redo(usize),
    /// An entry newer than the window, read from the journal.
    Above(usize),
}

impl UndoStack {
    /// Without a journal the stack is the whole history and keeps a cap; with
    /// one, entries beyond the window move to disk at the next sync instead.
    pub(super) fn cap_unjournaled(&mut self) {
        if self.journal.is_none() && self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
            self.dirty = true;
        }
    }

    /// A new edit after an undo discards the redo branch, on disk too. The
    /// discarded authors are kept only long enough for a conversation to learn
    /// its own edits are gone.
    pub(super) fn drop_redo(&mut self) {
        for entry in &self.redo {
            self.discarded.push((entry.id, entry.author.clone()));
        }
        if self.discarded.len() > super::MAX_UNDO {
            let excess = self.discarded.len() - super::MAX_UNDO;
            self.discarded.drain(..excess);
        }
        self.redo.clear();
        self.above = 0;
        self.dirty = true;
    }

    /// Applied entries in the whole history.
    pub fn applied_level(&self) -> usize {
        self.below + self.undo.len()
    }

    /// Entries in the whole history, applied or not.
    pub fn total_levels(&self) -> usize {
        self.below + self.undo.len() + self.redo.len() + self.above
    }

    /// The level entry `id` rests at, searching newest first so a mark near
    /// the cursor is cheap. This reads positions only, never an entry.
    pub fn level_of(&self, id: u64) -> Option<usize> {
        (1..=self.total_levels())
            .rev()
            .find(|&level| self.id_at_level(level - 1) == Some(id))
    }

    /// The applied entries in `from_level..to_level`, oldest first, as the
    /// author, cause and typed changes each one describes.
    pub fn entries_between(
        &self,
        from_level: usize,
        to_level: usize,
        properties: &ProjectProperties,
    ) -> Result<Vec<(Author, EditCause, oc_dsl::change::ChangeSet)>, String> {
        let applied = self.applied_level();
        if from_level > to_level || to_level > applied {
            return Err("The requested history levels are not applied".into());
        }
        (from_level..to_level)
            .map(|position| {
                let (author, cause, forward, inverse) = self
                    .applied_entry_at(position)
                    .ok_or_else(|| "An applied history entry could not be read".to_string())?;
                Ok((author, cause, entry_changes(&forward, &inverse, properties)))
            })
            .collect()
    }

    /// Where a zero-based position in the whole history lives.
    fn slot(&self, position: usize) -> Slot {
        if position < self.below {
            return Slot::Below(position);
        }
        let applied = self.below + self.undo.len();
        if position < applied {
            return Slot::Undo(position - self.below);
        }
        let after = applied + self.redo.len();
        if position < after {
            return Slot::Redo(self.redo.len() - 1 - (position - applied));
        }
        Slot::Above(position - after)
    }

    /// The id at a zero-based position in the whole history, from the window
    /// where it holds the entry and from the journal elsewhere.
    fn id_at_level(&self, position: usize) -> Option<u64> {
        match self.slot(position) {
            Slot::Below(position) | Slot::Above(position) => self.journal.as_ref()?.id_at(position),
            Slot::Undo(index) => self.undo.get(index).map(|entry| entry.id),
            Slot::Redo(index) => self.redo.get(index).map(|entry| entry.id),
        }
    }

    /// The cursor and the largest id the history holds, for a conversation to
    /// compare against on its next turn.
    pub fn mark(&self) -> HistoryMark {
        let applied = self.applied_level();
        let at = if applied == 0 {
            None
        } else {
            self.id_at_level(applied - 1)
        };
        let mut max_id = self.journal.as_ref().map(Journal::max_id).unwrap_or(0);
        for entry in self.undo.iter().chain(self.redo.iter()) {
            max_id = max_id.max(entry.id);
        }
        HistoryMark { at, max_id }
    }

    /// What changed since `mark` by anyone but `session_id`: the applied
    /// entries after its position, the steps undo took back, or the applied
    /// entries newer than a mark whose entry is gone.
    pub fn since(
        &self,
        mark: &HistoryMark,
        session_id: &str,
        properties: &ProjectProperties,
    ) -> Option<OutsideChanges> {
        let applied = self.applied_level();
        let found = match mark.at {
            None => Some(0),
            Some(id) => self.level_of(id),
        };
        let mut positions = Vec::new();
        let mut undone = 0;
        let mut rewritten = false;
        let mut pruned = false;
        match found {
            Some(position) if applied >= position => {
                // `position` counts entries up to the mark, so the new entries
                // start at that zero-based index. Newest first describes the
                // edits a reader needs.
                positions.extend((position..applied).rev());
            }
            Some(position) => {
                undone = position - applied;
            }
            None => {
                // Ids rise along the linear history. A mark below the oldest
                // entry it still holds was dropped by history cleanup; a mark
                // that is missing inside that range was replaced by an undo
                // branch. A history rewritten and later pruned past the mark
                // reads as pruned, because the whole range it named is gone.
                let oldest = self.id_at_level(0);
                let dropped = mark
                    .at
                    .is_some_and(|id| oldest.is_none_or(|oldest| id < oldest));
                if dropped {
                    pruned = true;
                    let at = mark.at.unwrap_or_default();
                    for position in (0..applied).rev() {
                        match self.id_at_level(position) {
                            Some(id) if id > at => positions.push(position),
                            _ => break,
                        }
                    }
                } else {
                    rewritten = true;
                    // Scanning back to the mark's largest id finds the entries
                    // it never saw.
                    for position in (0..applied).rev() {
                        match self.id_at_level(position) {
                            Some(id) if id > mark.max_id => positions.push(position),
                            _ => break,
                        }
                    }
                }
            }
        }
        let own = |author: &Author| matches!(author, Author::Agent { session: Some(session) } if session == session_id);
        // Own edits the person undid are invisible to the applied scan. Count
        // the ones the redo branch still holds and the ones a new edit
        // discarded, so the model never assumes its work still stands.
        let mut own_undone = 0usize;
        for (id, author) in &self.discarded {
            if *id > mark.max_id && own(author) {
                own_undone = own_undone.saturating_add(1).min(MAX_OUTSIDE_COUNT);
            }
        }
        for position in (applied..self.total_levels()).rev() {
            match self.id_at_level(position) {
                Some(id) if id > mark.max_id => {
                    if self.author_at(position).is_some_and(|author| own(&author)) {
                        own_undone = own_undone.saturating_add(1).min(MAX_OUTSIDE_COUNT);
                    }
                }
                _ => break,
            }
        }
        let mut listed = Vec::new();
        let mut read = 0;
        for &position in &positions {
            if listed.len() == MAX_OUTSIDE_ENTRIES {
                break;
            }
            read += 1;
            let Some((author, cause, forward, inverse)) = self.applied_entry_at(position) else {
                log::warn!("[history] cannot read an entry for the outside-changes notice");
                return None;
            };
            if !own(&author) {
                listed.push((author, cause, forward, inverse));
            }
        }
        // Entries past the listed ones are counted, not read: a long run of
        // edits outside the conversation costs index lookups only, while the
        // state lock is held. The count may include the conversation's own
        // edits from calls outside a turn.
        let more = positions.len() - read;
        let entries: Vec<OutsideEntry> = listed
            .into_iter()
            .map(|(author, cause, forward, inverse)| OutsideEntry {
                author,
                cause,
                changes: entry_changes(&forward, &inverse, properties),
            })
            .collect();
        if entries.is_empty() && undone == 0 && own_undone == 0 && !rewritten && !pruned {
            return None;
        }
        Some(OutsideChanges {
            entries,
            more,
            undone,
            own_undone,
            rewritten,
            pruned,
        })
    }

    /// One applied entry as its author, cause and forward-oriented patches.
    /// Applied positions live in the window or below it, never on the redo side.
    fn applied_entry_at(&self, position: usize) -> Option<(Author, EditCause, Vec<u8>, Vec<u8>)> {
        match self.slot(position) {
            Slot::Below(position) => {
                let entry = self
                    .journal
                    .as_ref()?
                    .read_range(position..position + 1)
                    .ok()?
                    .pop()?;
                Some((entry.author, entry.cause, entry.forward, entry.inverse))
            }
            Slot::Undo(index) => {
                let entry = self.undo.get(index)?;
                Some((
                    entry.author.clone(),
                    entry.cause,
                    entry.forward.clone(),
                    entry.inverse.clone(),
                ))
            }
            Slot::Redo(_) | Slot::Above(_) => None,
        }
    }

    /// The author at any history level, from the window or the journal.
    fn author_at(&self, position: usize) -> Option<Author> {
        match self.slot(position) {
            Slot::Below(position) | Slot::Above(position) => self
                .journal
                .as_ref()?
                .read_range(position..position + 1)
                .ok()?
                .pop()
                .map(|entry| entry.author),
            Slot::Undo(index) => self.undo.get(index).map(|entry| entry.author.clone()),
            Slot::Redo(index) => self.redo.get(index).map(|entry| entry.author.clone()),
        }
    }

    pub fn journaled(&self) -> bool {
        self.journal.is_some()
    }

    /// Whether the next undo (or redo) needs entries the window does not hold.
    pub(crate) fn needs_window(&self, undo: bool) -> bool {
        if undo {
            self.undo.is_empty() && self.below > 0
        } else {
            self.redo.is_empty() && self.above > 0
        }
    }

    /// The entry the last undo took back, as the footer describes it.
    pub fn undone_brief(&self, properties: &ProjectProperties) -> Option<Brief> {
        self.redo.last().map(|entry| Brief {
            changes: entry_changes(&entry.inverse, &entry.forward, properties),
            cause: entry.cause,
            author: entry.author.clone(),
        })
    }

    /// The entry the last redo applied again.
    pub fn redone_brief(&self, properties: &ProjectProperties) -> Option<Brief> {
        self.undo.last().map(|entry| Brief {
            changes: entry_changes(&entry.forward, &entry.inverse, properties),
            cause: entry.cause,
            author: entry.author.clone(),
        })
    }

    /// Use `journal` as the history and load the window around its cursor.
    /// The window it replaces is dropped: the journal is the history.
    pub(crate) fn attach_journal(&mut self, journal: Journal) -> Result<(), HistoryError> {
        NEXT_ENTRY_ID.fetch_max(
            journal.max_id().saturating_add(1),
            std::sync::atomic::Ordering::Relaxed,
        );
        let applied = journal.applied();
        let previous = self.journal.replace(journal);
        if let Err(error) = self.reload_window(applied) {
            self.journal = previous;
            return Err(error);
        }
        self.dirty = false;
        self.discarded.clear();
        Ok(())
    }

    /// Keep the window and write it into `journal` at the next sync. Used when
    /// the project gets a path (or a new one) and its history moves with it.
    pub(crate) fn adopt_journal(&mut self, journal: Journal, home: JournalHome) {
        NEXT_ENTRY_ID.fetch_max(
            journal.max_id().saturating_add(1),
            std::sync::atomic::Ordering::Relaxed,
        );
        if home == JournalHome::Fresh {
            // A fresh journal holds nothing beyond the window.
            self.below = 0;
            self.above = 0;
        }
        self.discarded.clear();
        self.journal = Some(journal);
        self.dirty = true;
    }

    /// Write the window's changes and the cursor to the journal, then move
    /// entries beyond the window out of memory. `head` is the document text at
    /// the cursor. Nothing leaves memory unless its write succeeded.
    pub(crate) fn sync_journal(&mut self, head: &str) -> Result<(), HistoryError> {
        let Some(journal) = self.journal.as_mut() else {
            return Ok(());
        };
        if self.below > journal.len() {
            // The journal lost entries the window counted on; the window's
            // own entries are rewritten from its new base.
            self.below = journal.len();
        }
        let base = self.below;
        let undo = &self.undo;
        let redo = &self.redo;
        let window = undo.len() + redo.len();
        let at = |offset: usize| -> (&UndoEntry, bool) {
            if offset < undo.len() {
                (&undo[offset], false)
            } else {
                (&redo[redo.len() - 1 - (offset - undo.len())], true)
            }
        };
        let mut common = 0;
        while common < window && journal.id_at(base + common) == Some(at(common).0.id) {
            common += 1;
        }
        if !(common == window && journal.len() == base + common + self.above) {
            journal.truncate(base + common).map_err(io_text)?;
            self.above = 0;
            let fresh: Vec<StoredEntry> = (common..window)
                .map(|offset| {
                    let (entry, redo_side) = at(offset);
                    stored(entry, redo_side)
                })
                .collect();
            journal.append(&fresh).map_err(io_text)?;
        }
        journal
            .write_state(base + undo.len(), head)
            .map_err(io_text)?;
        let excess = self.undo.len().saturating_sub(MAX_UNDO);
        if excess > 0 {
            self.undo.drain(..excess);
            self.below += excess;
        }
        let excess = self.redo.len().saturating_sub(MAX_UNDO);
        if excess > 0 {
            self.redo.drain(..excess);
            self.above += excess;
        }
        self.dirty = false;
        Ok(())
    }

    /// Replace the window with the journal's entries around `applied`. The
    /// window must already be synced; a read failure changes nothing.
    fn reload_window(&mut self, applied: usize) -> Result<(), HistoryError> {
        let journal = self.journal.as_ref().ok_or(HistoryError::NotJournaled)?;
        let len = journal.len();
        let applied = applied.min(len);
        let low = applied.saturating_sub(WINDOW_BELOW);
        let high = (applied + HISTORY_PAGE).min(len);
        let older = journal.read_range(low..applied).map_err(io_text)?;
        let newer = journal.read_range(applied..high).map_err(io_text)?;
        self.undo = older
            .into_iter()
            .map(|entry| from_stored(entry, false))
            .collect();
        self.redo = newer
            .into_iter()
            .rev()
            .map(|entry| from_stored(entry, true))
            .collect();
        self.below = low;
        self.above = len - high;
        self.merge_base = None;
        self.invalidate_gestures();
        Ok(())
    }

    /// Jump to a level outside the window by streaming patches from the
    /// journal, a page at a time, onto a copy of the project. The window must
    /// be synced. A failure leaves the project and the stack unchanged.
    pub(super) fn restore_from_journal(
        &mut self,
        level: usize,
        project: &mut Project,
    ) -> Result<Option<String>, HistoryError> {
        if self.is_dirty() {
            return Err(HistoryError::NotSynced);
        }
        let journal = self.journal.as_ref().ok_or(HistoryError::NotJournaled)?;
        let current = self.below + self.undo.len();
        let mut candidate = project.clone();
        let decode = |bytes: &[u8]| {
            compressed::decode_patch(bytes).map_err(|reason| {
                restore_text(format!(
                    "could not restore history at \"history entry\": {reason}"
                ))
            })
        };
        if level < current {
            let mut end = current;
            while end > level {
                let start = end.saturating_sub(HISTORY_PAGE).max(level);
                let entries = journal.read_range(start..end).map_err(io_text)?;
                let patches = entries
                    .iter()
                    .rev()
                    .map(|entry| decode(&entry.inverse))
                    .collect::<Result<Vec<_>, _>>()?;
                diff::apply_patches(&mut candidate, &patches).map_err(|reason| {
                    restore_text(format!("could not restore history: {reason}"))
                })?;
                end = start;
            }
        } else {
            let mut start = current;
            while start < level {
                let end = (start + HISTORY_PAGE).min(level);
                let entries = journal.read_range(start..end).map_err(io_text)?;
                let patches = entries
                    .iter()
                    .map(|entry| decode(&entry.forward))
                    .collect::<Result<Vec<_>, _>>()?;
                diff::apply_patches(&mut candidate, &patches).map_err(|reason| {
                    restore_text(format!("could not restore history: {reason}"))
                })?;
                start = end;
            }
        }
        self.reload_window(level)?;
        self.dirty = true;
        *project = candidate;
        Ok(self.undo.last().map(|entry| entry.tag.clone()))
    }

    /// Up to `limit` (at most one page) entries older than level `before`,
    /// newest first. With a journal the window must be synced. Only the
    /// returned entries decode their patches.
    pub fn history_page(
        &self,
        before: Option<usize>,
        limit: usize,
        properties: &ProjectProperties,
    ) -> Result<HistoryPage, HistoryError> {
        let total = self.total_levels();
        let applied = self.applied_level();
        let end = before.unwrap_or(total).min(total);
        let start = end.saturating_sub(limit.clamp(1, HISTORY_PAGE));
        let entries = match self.journal.as_ref() {
            Some(journal) => journal
                .read_range(start..end)
                .map_err(io_text)?
                .into_iter()
                .enumerate()
                .map(|(offset, entry)| page_entry(start + offset, applied, entry, properties))
                .rev()
                .collect(),
            None => {
                let all = self.history(properties);
                all[start..end].iter().rev().cloned().collect()
            }
        };
        Ok(HistoryPage {
            entries,
            total,
            applied,
            journaled: self.journal.is_some(),
        })
    }

    pub fn history_info(&self) -> HistoryInfo {
        HistoryInfo {
            total: self.total_levels(),
            applied: self.applied_level(),
            journaled: self.journal.is_some(),
            bytes: self.journal.as_ref().map(Journal::bytes).unwrap_or(0),
            path: self
                .journal
                .as_ref()
                .map(|journal| journal.dir().display().to_string()),
        }
    }

    /// Keep the newest `keep` entries; entries above the cursor always stay.
    /// The window must be synced. Returns how many entries were dropped.
    pub(crate) fn clear_older(&mut self, keep: usize) -> Result<usize, HistoryError> {
        if self.journal.is_none() {
            let drop = self
                .total_levels()
                .saturating_sub(keep)
                .min(self.undo.len());
            self.undo.drain(..drop);
            self.dirty = true;
            return Ok(drop);
        }
        if self.is_dirty() {
            return Err(HistoryError::NotSynced);
        }
        let journal = self.journal.as_mut().expect("the journal was checked");
        let applied = journal.applied();
        let dropped = journal.clear_older(keep).map_err(io_text)?;
        self.reload_window(applied - dropped)?;
        self.dirty = false;
        Ok(dropped)
    }

    /// Whether the in-memory window has changes the journal does not hold yet.
    /// A window the journal never received must not be replaced from it, or
    /// undo would apply an inverse to the wrong document.
    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty
    }
}

impl AppStateInner {
    /// Write the history's changes to disk. Skipped while a batch
    /// is open (its entries are provisional until it commits) and while a
    /// preview holds the document (neither side is at a history cursor).
    pub(crate) fn sync_history(&mut self) {
        if let Err(error) = self.try_sync_history() {
            log::warn!("[history] {error}");
        }
    }

    /// Write the history's changes to disk, reporting whether the journal now
    /// holds the window and the cursor. A caller that reloads the window from
    /// the journal must refuse when this fails.
    pub(crate) fn try_sync_history(&mut self) -> Result<(), HistoryError> {
        if self.transaction.is_some()
            || self.preview.is_some()
            || !self.undo.journaled()
            || self.history_move.is_some()
        {
            return Ok(());
        }
        self.undo.sync_journal(&self.dsl_source)?;
        Ok(())
    }

    /// Whether the history has changes the journal does not hold yet.
    pub(crate) fn history_due(&self) -> bool {
        self.undo.journaled()
            && self.transaction.is_none()
            && self.preview.is_none()
            && self.history_move.is_none()
            && self.undo.is_dirty()
    }

    /// Load the window the next undo (or redo) needs from disk. Inside a batch
    /// the window cannot move, because the batch's marks index into it.
    pub(crate) fn ensure_history_window(&mut self, undo: bool) -> Result<(), HistoryError> {
        if !self.undo.needs_window(undo) {
            return Ok(());
        }
        if self.transaction.is_some() {
            return Err(HistoryError::InBatch);
        }
        self.try_sync_history()?;
        if self.undo.is_dirty() {
            return Err(HistoryError::NotSynced);
        }
        let applied = self.undo.applied_level();
        self.undo.reload_window(applied)?;
        self.undo.dirty = false;
        Ok(())
    }

    /// Open (or start) the project's history, after the document was
    /// installed. The history is found by the file's path; a file the app has
    /// none for gets the one whose recorded fingerprint matches its bytes, so
    /// a project moved or copied outside the app keeps its history. When the
    /// file differs from the document the history last recorded, one entry
    /// bridges the two, so every older entry still undoes onto the document
    /// it was recorded against.
    pub(crate) fn attach_history(&mut self) {
        let Some(path) = self.file_path.clone() else {
            return;
        };
        let file_sha = crate::project_data::content_sha256(&path);
        let dir = journal::dir_for(&path);
        if journal::read_marker(&dir).is_none()
            && let Some(sha) = file_sha.as_deref()
            && let Some(found) = journal::find_by_fingerprint(sha, &dir)
        {
            // A history whose file is gone moves with the file; one whose file
            // is still there is copied, like a Save As. The history and every
            // other store follow the same relocation.
            let how = if crate::project_data::is_gone(&found.marker.path) {
                Relocation::Move
            } else {
                Relocation::Copy
            };
            let report = crate::project_data::relocate(&found.marker.path, &path, how);
            if report.history.is_failure() {
                log::warn!(
                    "[history] the history of {} stays where it was",
                    found.marker.path.display()
                );
            } else {
                log::info!(
                    "[history] found the history of {} by its content",
                    found.marker.path.display()
                );
            }
        }
        self.record_written(&path, file_sha);
        let mut journal = match Journal::open(&path) {
            Ok(journal) => journal,
            Err(error) => {
                log::warn!("[history] {}", io_text(error));
                return;
            }
        };
        let head = journal
            .head()
            .and_then(|head| crate::commands::installed_project(head).ok());
        if head.is_none() && journal.len() > 0 {
            // Patches without the document they apply to cannot be undone.
            log::warn!("[history] the history has no usable document; starting a new one");
            if let Err(error) = journal.truncate(0) {
                log::warn!("[history] {}", io_text(error));
                return;
            }
        }
        if let Err(error) = self.undo.attach_journal(journal) {
            log::warn!("[history] {error}");
            self.undo.clear();
            return;
        }
        if let Some(head) = head
            && head != self.project
        {
            let bridge = UndoEdit::new("external change", UndoScope::Project).by(Author::Outside);
            if let Err(error) = self.undo.push(head, &self.project, bridge) {
                log::warn!("[history] {error}");
            }
        }
        self.sync_history();
    }

    /// Start an empty history for a new document at `path`, so an open never
    /// adopts another project's history by content.
    pub(crate) fn start_history(&mut self, path: &std::path::Path) {
        if let Err(error) = journal::reset(path) {
            log::warn!("[history] {}", io_text(error));
        }
    }

    /// Record the content SHA-256 of the file the app just wrote, so its
    /// history is found if the file moves outside the app. The caller holds the
    /// document lock, so the marker write cannot race a history rewrite.
    pub(crate) fn record_written(&mut self, path: &std::path::Path, sha256: Option<String>) {
        let dir = journal::dir_for(path);
        if let Err(error) = journal::Marker::new(path, sha256).write(&dir) {
            log::warn!("[history] cannot record {}: {error}", path.display());
        }
    }

    /// Give the history its home at the document's new path. Relocation has
    /// already put the files there; this opens the journal and adopts the
    /// window, then writes any entries the move held back.
    pub(crate) fn rehome_history(&mut self, home: JournalHome, sha256: Option<String>) {
        let Some(path) = self.file_path.clone() else {
            return;
        };
        self.record_written(&path, sha256);
        match Journal::open(&path) {
            Ok(journal) => self.undo.adopt_journal(journal, home),
            Err(error) => log::warn!("[history] {}", io_text(error)),
        }
        self.history_move = None;
        self.sync_history();
    }

    /// Finish a relocation that ran after the document lock was released.
    pub(crate) fn finish_history_move(
        &mut self,
        report: crate::project_data::RelocationReport,
        sha256: Option<String>,
    ) {
        self.rehome_history(report.history_home(), sha256);
    }
}
