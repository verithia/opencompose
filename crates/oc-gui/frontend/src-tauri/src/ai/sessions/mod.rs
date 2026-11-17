//! Saved AI conversations under the configuration root's `ai/sessions`
//! directory: one directory per workspace key, one directory per session, and
//! an append-only `messages.jsonl`.
//!
//! Layout:
//!
//! ```text
//! sessions/
//!   <workspace key>/workspace.json  the directory this key was derived from
//!   <workspace key>/<session id>/meta.json
//!   <workspace key>/<session id>/messages.jsonl
//!   <workspace key>/<session id>/view.json
//!   <workspace key>/<session id>/results/<sha256>.txt
//!   <workspace key>/<session id>/versions/<sha256>.ocp
//!   detached/<session id>/...       a conversation with no workspace
//!   audition-media/                 clips a listening request replays
//!   audition-renders/               full-rate renders kept for listening
//! ```
//!
//! The key is the first 8 bytes of the SHA-256 of the canonical workspace
//! directory path as hex, so the directory name is stable and does not leak the
//! path. `workspace.json` keeps the path so a moved workspace can be found
//! again.
//!
//! `messages.jsonl` holds one record per model turn. A record is appended whole
//! (`O_APPEND`, one write); the file is rewritten only when the size cap trims
//! it or the history before a compaction is dropped. Limits are stated with the
//! constants below and enforced in `append`. `view.json` names the newest
//! record that replaced the conversation, so a read parses only the current
//! view; the panel reads that view a page of whole turns at a time.
//!
//! A tool result too large for its message row is written to
//! `results/<sha256>.txt`; the row keeps `result_sha` in place of the text. The
//! sweep removes a result file no saved line of its session names, a fork
//! copies the files its inherited rows name, and a read validates the hash and
//! bounds the bytes it loads.
mod artifacts;
mod summary;
pub(crate) use summary::SUMMARY_OUTPUT_TOKENS;
use summary::{
    PlanInputs, plan_compaction, plan_compaction_request, planned_messages, should_compact,
    view_digest,
};
#[cfg(test)]
use summary::{
    compaction_input, retained_comparison_pair, retained_media_row, summary_metadata, summary_text,
};
mod audition_archive;
mod binding;
#[cfg(test)]
mod binding_tests;
#[cfg(test)]
mod compaction_tests;
mod journal;
pub(super) mod lookup;
pub(crate) mod message_protocol;
#[cfg(test)]
mod test_support;
pub mod tool_results;
pub mod turns;
pub(crate) mod versions;
mod view;
mod workspace;
#[cfg(test)]
pub(crate) use test_support::{TestRootGuard, test_root};
mod row;
mod tokens;
use super::host_notice::{BindCause, HostNotice, PendingBinding, ToolSurface};
use super::session_state::{self, SessionState, StateAuthor};
#[cfg(test)]
use crate::edit_history::HistoryMark;
use crate::private_fs;
pub use audition_archive::{
    AUDITION_ARCHIVE_BUDGET, AuditionArchive, EvictedAudition, audition_archive, bound_media_replay,
};
use audition_archive::{
    AUDITION_RENDER_STAGE, clip_dir, persist_auditions, render_dir, session_references, sweep,
};
#[cfg(test)]
use audition_archive::{ArchiveSweep, archived_bytes, mp3_bitrate};
pub use binding::{BindingMismatch, BindingStatus, BoundProject, SavedHistoryMark, binding_status};
#[cfg(test)]
use journal::append_records;
#[cfg(test)]
use journal::newest_that_fit;
use journal::{
    AppendOutcome, Dropped, Record, RecordKind, VERSION, append_records_under, compaction_indices,
    read_current, read_meta, read_records, read_view, write_meta, write_records,
};
pub use journal::{read_meta_for_storage, read_original};
pub use message_protocol::MessageKind;
use message_protocol::{MessageMeta, SummaryMeta};
use row::{
    CONTEXT_AUDITION, CONTEXT_SUMMARY, MAX_PROJECT_PATH, canonical_media, canonicalize_media,
    mark_missing_media, plain_message, same_turn, saved_message, strips_replay_state,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(test)]
pub use tokens::IMAGE_TOKENS;
pub use tokens::{estimate_text_tokens, estimate_tokens, media_tokens, value_tokens};
pub(crate) use versions::HistoryAt;
pub use view::MAX_PAGE_TURNS;
pub use workspace::Scope;
use workspace::{canonical_dir, hashed_key, is_valid_key};

/// Session store directory name.
pub const DIR_NAME: &str = "sessions";
/// Key for a session not bound to a project.
pub const DETACHED_KEY: &str = "detached";
/// The folder under the sessions root that holds per-conversation locks. It
/// sits outside every workspace and session directory, so deleting or renaming
/// a conversation never takes its lock file with it.
pub const LOCKS_DIR: &str = "locks";
/// Session metadata file.
pub const META_FILE: &str = "meta.json";
/// Session transcript file.
pub const MESSAGES_FILE: &str = "messages.jsonl";
/// Session workspace file.
pub const WORKSPACE_FILE: &str = "workspace.json";

/// Sessions kept per workspace; creating another one deletes the least recently
/// updated. A deleted session's directory is removed with it.
pub const MAX_SESSIONS_PER_WORKSPACE: usize = 32;
/// `messages.jsonl` upper bound. An append that would exceed it drops the oldest
/// messages into a `trim` record first, so one session cannot grow without limit.
pub const MAX_SESSION_BYTES: u64 = 1024 * 1024 * 1024;
/// The smallest share of the context window at which auto-compaction fires.
pub const COMPACTION_MIN_FRACTION: f64 = 0.6;
/// Space held back for the compaction prompt when the prompt itself is small.
pub const COMPACTION_RESERVE_FLOOR: u64 = 1000;
/// The target a compaction aims for, as a share of the context window.
pub const AUTO_COMPACT_TARGET_FRACTION: f64 = 0.5;

/// The token size at which a request compacts.
///
/// The threshold leaves room for the output the model may write and for the
/// compaction prompt, but never drops below the share that keeps almost half
/// the window free. A window of 0 or 1 means unknown and is the caller's to
/// reject before calling this.
pub fn compaction_threshold(window: u64, max_output: u64, prompt_tokens: u64) -> u64 {
    let reserve = prompt_tokens.max(COMPACTION_RESERVE_FLOOR);
    let headroom = window.saturating_sub(max_output).saturating_sub(reserve);
    let share = (window as f64 * COMPACTION_MIN_FRACTION) as u64;
    share.max(headroom).max(1)
}
/// Tool pairs carried into the summary as plain text.
pub const RETAINED_TOOL_PAIRS: usize = 6;
/// The line the generated tool digest starts with; legacy summaries carry it
/// inside their content.
const SUMMARY_DIGEST_PREFIX: &str = "Retained recent tool calls (newest ";
/// The wire accepts 100 messages per request; compact before a saved session
/// gets close, so a restored conversation is always sendable.
pub const MAX_AUTO_COMPACT_MESSAGES: usize = 80;
/// Title bound; the first turn's text is the default title.
pub const MAX_TITLE_CHARS: usize = 80;
const MAX_SUMMARY_RESULT_CHARS: usize = 200;
const MAX_SUMMARY_ITEM_CHARS: usize = 160;
const MAX_INTENT_CHARS: usize = 600;

static TEST_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);
#[cfg(test)]
static TEST_ROOT_LOCK: Mutex<()> = Mutex::new(());
static ID_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What a call that changes the project DSL may do without asking.
///
/// The wire names are `require_approval`, `autoreview` and `write`. The default
/// is `write` for now; it returns to `require_approval` once the review prompt
/// shows a human-readable change instead of raw DSL.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPermission {
    /// Pause every DSL-modifying call and wait for approve or reject.
    RequireApproval,
    /// Apply the change in the turn, after validating it and showing the diff.
    Autoreview,
    #[default]
    Write,
}

/// One saved conversation's metadata. `tokens` is the current estimate of the
/// saved conversation, not a provider's reported usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub created: String,
    pub updated: String,
    pub model: String,
    pub provider: String,
    /// Directory the session belongs to; `detached` when there is none.
    pub workspace_key: String,
    /// The project file this conversation is bound to. A turn while another
    /// project is open is refused until the person binds the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<BoundProject>,
    /// A binding the next admitted turn tells the model about, as one typed
    /// host notice before that turn's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_binding: Option<PendingBinding>,
    /// A goal change a person made while the conversation had an open round.
    /// The next admitted turn writes it before that turn's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_state: Option<SessionState>,
    /// The project's history cursor after this conversation's previous turn,
    /// so the next turn can tell the model what changed outside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_mark: Option<SavedHistoryMark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// What a call that changes the project DSL may do without asking. A
    /// session saved before this field reads as the default.
    #[serde(default)]
    pub permission: SessionPermission,
    /// The tools and permission a turn last offered, so a later turn tells the
    /// model what changed. A session saved before this field reads as `None`
    /// and records its first surface silently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_surface: Option<ToolSurface>,
    pub turns: u64,
    pub tokens: u64,
    /// Whether a person or the title pass named this session. Until then the
    /// title is the first message and may be replaced with a short summary.
    #[serde(default)]
    pub named: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_at: Option<String>,
    /// Set when the session was forked from another one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// A conversation copied into a new session.
#[derive(Debug, Clone, Copy)]
pub struct Fork<'a> {
    /// The saved conversation to copy.
    pub parent: &'a str,
    /// Copy only this many saved messages from the start; `None` copies all.
    pub through: Option<usize>,
}

/// What the project looked like when a summary was written: the alternate to
/// carrying the DSL itself into the summary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectFacts {
    pub path: Option<String>,
    pub dsl_digest: String,
    pub track_count: usize,
}

/// One loaded conversation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Loaded {
    pub meta: SessionMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<super::session_usage::SessionUsageSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_error: Option<super::session_usage::AccountingError>,
    pub messages: Vec<Value>,
    /// Records this build could not read; a corrupt line never hides the rest.
    pub skipped: u64,
    /// Whether a compaction replaced older turns, so the panel can offer the
    /// history behind the summary.
    pub compacted: bool,
}

/// A conversation opened at its newest turns.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoadedPage {
    #[serde(flatten)]
    pub loaded: Loaded,
    /// Where the loaded messages start in the whole view.
    pub start: usize,
    /// How many messages the whole view holds.
    pub total: usize,
    /// The view's shape; a page asked for under another one is stale.
    pub revision: String,
    /// The panel's estimate of the messages before `start`.
    pub older_tokens: u64,
    /// Current goal and plan changes from the whole view, including clears.
    pub state: Vec<SessionState>,
}

/// Older turns of a conversation, in view order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Page {
    pub start: usize,
    pub total: usize,
    pub revision: String,
    pub older_tokens: u64,
    pub messages: Vec<Value>,
}

/// A page, or the answer that the view changed shape since the panel read it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PageRead {
    Page(Page),
    Stale,
}

/// The outcome of folding a conversation into a summary.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Compaction {
    pub id: String,
    pub compacted_at: String,
    /// The conversation after compaction, ready to replay.
    pub messages: Vec<Value>,
    pub dropped_from: usize,
    pub dropped_to: usize,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub retained_pairs: usize,
}

/// What clearing old conversation history dropped and freed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ClearReport {
    pub sessions: usize,
    pub dropped_records: u64,
    pub removed_assets: u64,
    pub freed_bytes: u64,
}

/// Why a model-written compaction did not produce a summary. Cancellation
/// leaves the current view intact; other failures use the mechanical summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionFailure {
    ProviderError,
    Cancelled,
    Empty,
    Conflict,
    /// The store could not publish the summary. The panel shows a failed
    /// summary, not a provider problem.
    Store,
    /// The model text was too large or did not reduce the conversation more
    /// than the mechanical fallback.
    Rejected,
}

impl CompactionFailure {
    /// The reason a notice can carry; cancellation is not one.
    pub fn notice_reason(self) -> Option<Self> {
        (self != Self::Cancelled).then_some(self)
    }
}

/// What publishing a planned compaction produced: the committed compaction
/// when one was written, and why model text was replaced when it was not used.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResolution {
    pub compaction: Option<Compaction>,
    pub failure: Option<CompactionFailure>,
}

/// What a committed compaction wrote, and why its model text was replaced when
/// it was not used.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionCommit {
    pub compaction: Compaction,
    pub summary_failure: Option<CompactionFailure>,
}

/// What a compaction that needs a model call decides under the store lock.
///
/// The plan holds the dropped part as plain text, the deterministic fallback
/// and the pieces of the first message the summary fills in. It writes nothing:
/// the commit appends one compaction record only when the view still matches.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPlan {
    pub(crate) id: String,
    pub(crate) view_digest: String,
    pub(crate) dropped_from: usize,
    pub(crate) dropped_to: usize,
    pub(crate) retained_pairs: usize,
    pub(crate) tokens_before: u64,
    pub(crate) target_tokens: u64,
    pub(crate) compacted_at: String,
    /// The dropped turns as plain text, bounded for one request.
    pub(crate) input: String,
    /// The header and digest a model answer is placed between.
    pub(crate) summary: summary::SummaryParts,
    /// The mechanical summary the store writes when no model answer is used.
    pub(crate) fallback: Vec<Value>,
    /// What the project looked like when the plan was built, so a retry can
    /// write the same deterministic header without re-reading the project.
    pub(crate) facts: ProjectFacts,
}

/// What a refused reshaping of a conversation (compact, drop history, delete)
/// is about. The panel tells the cases apart by this, never by the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionErrorKind {
    /// A turn is writing this conversation.
    Busy,
    /// The conversation changed under the request: another window already
    /// did what this one asked for.
    Conflict,
    /// Another process holds this conversation's lock.
    #[serde(rename = "session_in_use")]
    InUse,
    /// The store refused or failed for any other reason.
    SessionStore,
}

/// A typed refusal of a session command, serialized as `{kind, message}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionError {
    pub kind: SessionErrorKind,
    pub message: String,
}

impl SessionError {
    pub fn new(kind: SessionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<String> for SessionError {
    fn from(message: String) -> Self {
        Self::new(SessionErrorKind::SessionStore, message)
    }
}

impl From<SessionError> for String {
    fn from(error: SessionError) -> Self {
        error.message
    }
}

/// Why preparation refused before saving anything. The binding case is decided
/// under the store lock; the admitted case is the request owner's refusal of
/// the planned request.
#[derive(Debug, Clone, PartialEq)]
pub enum PrepareFailure<E> {
    Binding(BindingMismatch),
    Admitted(E),
    Store(String),
}

impl<E> From<String> for PrepareFailure<E> {
    fn from(message: String) -> Self {
        Self::Store(message)
    }
}

/// A check the store runs while it holds its mutation lock, right before a
/// reshaping write, so the condition it tests cannot change in between.
pub type Guard<'a> = &'a dyn Fn() -> Result<(), SessionError>;

/// Shared context for saving and compacting one model turn.
pub struct PrepareContext<'a> {
    /// The current document, captured with the request's project identity.
    pub snapshot: Option<&'a str>,
    /// The edit-history position the document was at, beside its snapshot.
    pub history: Option<&'a HistoryAt>,
    pub overhead: u64,
    pub context_length: u64,
    pub max_output: u64,
    pub prompt_tokens: u64,
    pub facts: &'a ProjectFacts,
    pub provider: &'a str,
    pub model: &'a str,
    /// What changed outside this conversation since its previous turn, saved
    /// as a typed notice before the turn's message.
    pub outside: Option<HostNotice>,
    /// The tools and permission this turn offers, compared with the last
    /// saved surface before the turn's message.
    pub surface: Option<ToolSurface>,
}

/// One autosave step: what the store now holds and whether it compacted.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub meta: SessionMeta,
    /// The saved conversation, without the system prompt.
    pub messages: Vec<Value>,
    /// The request to send when the client's own messages must survive: an
    /// inline audio clip is not saved, so the store cannot rebuild that turn.
    pub live: Option<Vec<Value>>,
    pub appended: usize,
    pub compacted: Option<Compaction>,
    /// A compaction the turn needs but whose summary call runs after the lock
    /// is released. `messages` stays the uncompacted view until it commits.
    pub pending_compaction: Option<CompactionPlan>,
    /// Why the automatic compaction this turn needed did not happen. The turn
    /// still goes out; the caller tells the panel.
    pub compaction_error: Option<CompactionFailure>,
    /// Archive files this turn's auditions evicted to stay inside the budget.
    pub evicted_auditions: Vec<EvictedAudition>,
    /// Audition rows added by this request, for the live operator transcript.
    pub saved_auditions: Vec<Value>,
}

impl Prepared {
    /// Replaces the saved view with the compaction that was just published.
    pub fn apply(&mut self, compaction: &Compaction) {
        self.messages = compaction.messages.clone();
        self.meta.tokens = compaction.tokens_after;
        self.meta.compacted_at = Some(compaction.compacted_at.clone());
        self.compacted = Some(compaction.clone());
    }
}

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    lookup: Arc<dyn lookup::Io>,
    /// The largest `messages.jsonl` this store writes; tests lower it to
    /// exercise the trim path without a gigabyte of data.
    session_cap: u64,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            lookup: Arc::new(lookup::Disk),
            session_cap: MAX_SESSION_BYTES,
        }
    }

    /// The operator's store: `~/.opencompose/ai/sessions`.
    pub fn config() -> Self {
        if let Ok(root) = TEST_ROOT.lock()
            && let Some(root) = root.as_ref()
        {
            return Self::new(root.clone());
        }
        Self::new(private_fs::config_dir().join("ai").join(DIR_NAME))
    }

    pub(super) fn with_usage<T>(
        &self,
        id: &str,
        apply: impl FnOnce(&Path) -> Result<T, super::session_usage::AccountingError>,
    ) -> Result<T, super::session_usage::AccountingError> {
        let _lock = self
            .mutation_lock_io()
            .map_err(super::session_usage::AccountingError::io)?;
        let (dir, _) =
            lookup::find(self.lookup.as_ref(), &self.root, id).map_err(|error| match error {
                lookup::Error::Io(error) => super::session_usage::AccountingError::io(error),
                lookup::Error::Corrupt(error) => {
                    super::session_usage::AccountingError::format(error)
                }
                other => super::session_usage::AccountingError::missing(other),
            })?;
        apply(&dir)
    }

    /// The directory sessions live in; the audition archive sits beside them.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The lock file that keeps one conversation open in one process.
    pub fn lock_path(&self, id: &str) -> PathBuf {
        self.root.join(LOCKS_DIR).join(format!("session-{id}.lock"))
    }

    /// Take this conversation's lock, without waiting.
    pub fn acquire_lock(
        &self,
        id: &str,
    ) -> Result<crate::process_lock::ProcessLock, crate::process_lock::LockError> {
        if !is_valid_id(id) {
            return Err(crate::process_lock::LockError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid session id",
            )));
        }
        crate::process_lock::ProcessLock::acquire(self.lock_path(id))
    }

    // ---- workspace keys -------------------------------------------------

    // Asset publication and the transcript that names the assets form one
    // operation from the collector's perspective.
    fn mutation_lock(&self) -> Result<std::fs::File, String> {
        self.mutation_lock_io()
            .map_err(|error| format!("Could not lock saved conversations: {error}"))
    }
    fn mutation_lock_io(&self) -> std::io::Result<std::fs::File> {
        private_fs::ensure_dir(&self.root)?;
        let lock = private_fs::lock(
            &self.root.join(".sessions.lock"),
            private_fs::LockMode::Exclusive,
            private_fs::LockWait::Wait,
        )?;
        // Only a stopped writer can leave a stage while the lock is held.
        let _ = std::fs::remove_file(self.root.join(AUDITION_RENDER_STAGE));
        Ok(lock)
    }

    /// Appends records to a conversation under this store's byte cap. Every
    /// write of a turn, compaction or trim goes through here.
    fn append(&self, dir: &Path, records: &[Record]) -> Result<AppendOutcome, String> {
        append_records_under(dir, records, self.session_cap)
    }

    // ---- sessions -------------------------------------------------------

    pub fn find(&self, id: &str) -> Result<(PathBuf, SessionMeta), String> {
        lookup::find(self.lookup.as_ref(), &self.root, id).map_err(|error| error.to_string())
    }

    /// Starts a session in `workspace`, working on `project`. Either may be
    /// absent: a session with no workspace lives under `detached`.
    pub fn create(
        &self,
        workspace: Option<&Path>,
        project: Option<&Path>,
        title: Option<&str>,
        fork: Option<Fork<'_>>,
    ) -> Result<SessionMeta, String> {
        let _lock = self.mutation_lock()?;
        let at = now();
        let key = self.workspace_key(workspace);
        self.ensure_workspace(&key, workspace, &at)?;
        // A fork copies the parent's saved conversation; the parent stays read-only.
        let (inherited, parent) = match fork.filter(|fork| is_valid_id(fork.parent)) {
            Some(fork) => {
                let (_, parent_meta) = self
                    .find(fork.parent)
                    .map_err(|_| format!("no session '{}' to fork", fork.parent))?;
                let mut messages = self.view(fork.parent)?;
                if let Some(through) = fork.through {
                    // A cut that names no message is refused: an empty fork or
                    // one past the end would otherwise read as a whole copy.
                    if through == 0 || through > messages.len() {
                        return Err(format!(
                            "session '{}' has no message {through} to fork from",
                            fork.parent
                        ));
                    }
                    messages.truncate(through);
                    // A fork replays its wire, so it must not end on a call
                    // that only exists to be answered.
                    trim_unanswered_call(&mut messages);
                    if messages.is_empty() {
                        return Err(format!(
                            "session '{}' has no completed turn to fork from",
                            fork.parent
                        ));
                    }
                }
                (Some(messages), Some(parent_meta))
            }
            None => (None, None),
        };
        let forked_title = parent
            .as_ref()
            .map(|parent| format!("{} (fork)", parent.title));
        let parent_from = parent.as_ref().and_then(|parent| {
            parent
                .pending_binding
                .as_ref()
                .and_then(|pending| pending.from.clone())
                .or_else(|| parent.project_path.as_ref().map(BoundProject::to_string))
        });
        let project_path = project.map(BoundProject::of);
        // A fork of a conversation bound to another file starts on the chosen
        // file like a rebind. It names the path the model last saw, which is an
        // unsent binding when the parent still owes one. A fork with no project
        // has no boundary to explain.
        let pending_binding = match (&project_path, parent_from) {
            (Some(to), Some(from)) if from != to.as_str() => Some(PendingBinding {
                from: Some(from),
                cause: BindCause::Chosen,
            }),
            _ => None,
        };
        let meta = SessionMeta {
            id: new_id(),
            title: clean_title(title.or(forked_title.as_deref()).unwrap_or_default()),
            created: at.clone(),
            updated: at.clone(),
            model: parent
                .as_ref()
                .map(|parent| parent.model.clone())
                .unwrap_or_default(),
            provider: parent
                .as_ref()
                .map(|parent| parent.provider.clone())
                .unwrap_or_default(),
            workspace_key: key.clone(),
            project_path,
            pending_binding,
            pending_state: None,
            history_mark: None,
            workspace: workspace.map(|path| canonical_dir(path).to_string_lossy().to_string()),
            // A fork keeps the mode the conversation was working under.
            permission: parent
                .as_ref()
                .map(|parent| parent.permission)
                .unwrap_or_default(),
            tool_surface: None,
            turns: 0,
            tokens: inherited.as_deref().map(estimate_tokens).unwrap_or(0),
            named: false,
            compacted_at: None,
            parent: parent.as_ref().map(|parent| parent.id.clone()),
        };
        let dir = self.root.join(&key).join(&meta.id);
        private_fs::ensure_dir(&dir).map_err(|error| format!("sessions: {error}"))?;
        let created = (|| {
            if let Some(inherited) = inherited.filter(|messages| !messages.is_empty()) {
                if let Some(parent) = &parent {
                    let (parent_dir, _) = self.find(&parent.id)?;
                    versions::copy_references(&parent_dir, &dir, &inherited)?;
                    tool_results::copy_references(&parent_dir, &dir, &inherited)?;
                }
                write_records(&dir, &[turn_record(&at, inherited)])?;
            }
            super::session_usage::initialize(&dir).map_err(|error| error.message)?;
            write_meta(&dir, &meta)
        })();
        if let Err(error) = created {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(error);
        }
        self.prune(&key, &meta.id)?;
        Ok(meta)
    }

    /// Deletes the least recently updated session of a workspace while it holds
    /// more than [`MAX_SESSIONS_PER_WORKSPACE`]. `keep` is never dropped.
    fn prune(&self, key: &str, keep: &str) -> Result<(), String> {
        let mut sessions: Vec<SessionMeta> = std::fs::read_dir(self.root.join(key))
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name().to_string_lossy().to_string();
                        is_valid_id(&name)
                            .then(|| read_meta(&entry.path().join(META_FILE)))
                            .flatten()
                    })
                    .collect()
            })
            .unwrap_or_default();
        if sessions.len() <= MAX_SESSIONS_PER_WORKSPACE {
            return Ok(());
        }
        sessions.sort_by(|left, right| {
            left.updated
                .cmp(&right.updated)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut excess = sessions.len() - MAX_SESSIONS_PER_WORKSPACE;
        let before = excess;
        for meta in sessions {
            if excess == 0 {
                break;
            }
            if meta.id == keep {
                continue;
            }
            let dir = self.root.join(key).join(&meta.id);
            if dir.starts_with(&self.root) && dir.is_dir() {
                let _ = std::fs::remove_dir_all(&dir);
                excess -= 1;
            }
        }
        // The shared audition files only the pruned conversations named go
        // with them.
        if excess < before {
            self.collect_unreferenced_locked();
        }
        Ok(())
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<SessionMeta, String> {
        self.rename_owned(id, title, None)
    }

    pub fn rename_owned(
        &self,
        id: &str,
        title: &str,
        incarnation: Option<&str>,
    ) -> Result<SessionMeta, String> {
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        super::session_usage::verify_at(&dir, incarnation).map_err(|error| error.message)?;
        meta.title = clean_title(title);
        meta.named = true;
        meta.updated = now();
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    /// Sets what this session may do to the DSL without asking. The setting is
    /// per session: another conversation keeps its own.
    pub fn set_permission(
        &self,
        id: &str,
        permission: SessionPermission,
    ) -> Result<SessionMeta, String> {
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        meta.permission = permission;
        meta.updated = now();
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    /// Records a person's goal change. Each call appends one `session_state`
    /// row, so a rewind before it restores the previous goal. While the
    /// conversation has an open round the change waits in the metadata: a row
    /// may not follow calls that have no result yet, and the next admitted
    /// turn writes it before that turn's message.
    pub fn set_goal(
        &self,
        id: &str,
        text: Option<&str>,
        turn_running: bool,
    ) -> Result<SessionMeta, String> {
        let state = match text.map(session_state::trim_text) {
            None => SessionState::GoalCleared {
                author: StateAuthor::Person,
            },
            Some("") => {
                return Err("A goal needs some text.".into());
            }
            Some(text) => SessionState::GoalSet {
                text: text.to_owned(),
                author: StateAuthor::Person,
            },
        };
        if !state.valid() {
            return Err(format!(
                "A goal is at most {} characters.",
                session_state::MAX_GOAL_CHARS
            ));
        }
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        let (view, _) = read_view(&dir)?;
        let open_round = turn_running || !turns::open_calls(&view).is_empty();
        meta.updated = now();
        if open_round {
            meta.pending_state = Some(state);
            write_meta(&dir, &meta)?;
            return Ok(meta);
        }
        let row = session_state::state_row(&state);
        let written = self.append(&dir, &[turn_record(&now(), vec![row])])?;
        let (view, _) = read_view(&dir)?;
        meta.tokens = written.kept_tokens(&view);
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    /// [`Self::delete`] after `guard` admits it under the store lock.
    pub fn delete_guarded(&self, id: &str, guard: Guard<'_>) -> Result<(), SessionError> {
        let _lock = self.mutation_lock()?;
        guard()?;
        self.delete_inner(id)?;
        self.collect_unreferenced_locked();
        Ok(())
    }

    fn delete_inner(&self, id: &str) -> Result<(), String> {
        let (dir, _) = self.find(id)?;
        if !dir.starts_with(&self.root) || !dir.is_dir() {
            return Err("Invalid session directory".into());
        }
        std::fs::remove_dir_all(&dir).map_err(|error| format!("sessions: {error}"))
    }

    // ---- conversation ---------------------------------------------------

    pub fn load(&self, id: &str) -> Result<Loaded, String> {
        let (dir, meta) = self.find(id)?;
        let current = read_current(&dir)?;
        Ok(self.loaded(
            id,
            meta,
            current.messages,
            current.skipped,
            current.compacted,
        ))
    }

    fn loaded(
        &self,
        id: &str,
        meta: SessionMeta,
        mut messages: Vec<Value>,
        skipped: u64,
        compacted: bool,
    ) -> Loaded {
        mark_missing_media(&mut messages);
        let (usage, usage_error) =
            match self.with_usage(id, |dir| super::session_usage::snapshot_at(dir, id)) {
                Ok(usage) => (usage, None),
                Err(error) => (None, Some(error)),
            };
        Loaded {
            meta,
            usage,
            usage_error,
            messages,
            skipped,
            compacted,
        }
    }

    /// The conversation with only its newest `turns` turns, for the panel to
    /// open; older pages come from [`Store::page`].
    pub fn open_page(&self, id: &str, turns: usize) -> Result<LoadedPage, String> {
        let (dir, meta) = self.find(id)?;
        let mut current = read_current(&dir)?;
        let total = current.messages.len();
        let mut state = session_state::latest_states(&current.messages);
        // A change recorded while a round is open waits in the metadata for
        // the next turn; it is still the conversation's newest state, so the
        // page carries it as if its row were already written.
        if let Some(pending) = meta.pending_state.clone() {
            state.retain(|saved| saved.is_plan() != pending.is_plan());
            state.push(pending);
        }
        let start = view::page_start(&current.messages, total, turns);
        let page = current.messages.split_off(start);
        let older_tokens = view::panel_tokens(&current.messages);
        Ok(LoadedPage {
            loaded: self.loaded(id, meta, page, current.skipped, current.compacted),
            start,
            total,
            revision: current.revision,
            older_tokens,
            state,
        })
    }

    /// The page of whole turns that ends before message `before`, when the
    /// view still has the shape `revision` names.
    pub fn page(
        &self,
        id: &str,
        before: usize,
        turns: usize,
        revision: &str,
    ) -> Result<PageRead, String> {
        let (dir, _) = self.find(id)?;
        let mut current = read_current(&dir)?;
        if current.revision != revision || before > current.messages.len() {
            return Ok(PageRead::Stale);
        }
        let total = current.messages.len();
        let start = view::page_start(&current.messages, before, turns);
        current.messages.truncate(before);
        let mut messages = current.messages.split_off(start);
        mark_missing_media(&mut messages);
        Ok(PageRead::Page(Page {
            start,
            total,
            revision: current.revision,
            older_tokens: view::panel_tokens(&current.messages),
            messages,
        }))
    }

    fn collect_unreferenced_locked(&self) -> u64 {
        let (referenced, version_dirs) = session_references(&self.root);
        let mut removed = 0u64;
        sweep(&clip_dir(&self.root), &referenced, &mut removed);
        sweep(&render_dir(&self.root), &referenced, &mut removed);
        for dir in version_dirs {
            // The version folder's parent is the session, whose own lines own
            // its results.
            if let Some(session) = dir.parent() {
                removed += tool_results::collect(session);
            }
            sweep(&dir, &referenced, &mut removed);
        }
        removed
    }

    /// Remove every archived audition file, referenced or not, and answer how
    /// many went. Conversations keep the paths their messages name; a later load
    /// reports the files missing instead of dropping the reference.
    pub fn clear_audition_archive(&self) -> Result<u64, String> {
        let _lock = self.mutation_lock()?;
        let mut removed = 0u64;
        for dir in [clip_dir(&self.root), render_dir(&self.root)] {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_file() && std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Save one DSL version for a session. Content-addressed, so the same
    /// document costs one file however many messages carry it.
    pub fn snapshot_dsl(&self, id: &str, dsl: &str) -> Result<String, String> {
        let _lock = self.mutation_lock()?;
        let (dir, _) = self.find(id)?;
        versions::save(&dir, dsl)
    }

    /// The DSL a snapshot holds. The hash is validated as hex before it is used
    /// as a file name, so a caller cannot address another file.
    pub fn dsl_version(&self, id: &str, hash: &str) -> Result<String, String> {
        let (dir, _) = self.find(id)?;
        versions::read(&dir, hash)
    }

    /// The history before compaction, for the panel's original view.
    pub fn original(&self, id: &str) -> Result<(Vec<Value>, u64), String> {
        let (dir, _) = self.find(id)?;
        let (mut messages, compactions, _) = read_original(&dir)?;
        mark_missing_media(&mut messages);
        Ok((messages, compactions))
    }

    /// The full result behind the newest saved preview of `call` in this
    /// session's current view, read from disk; `None` when no saved row keeps
    /// one.
    #[cfg(test)]
    pub fn tool_result(&self, id: &str, call: &str) -> Result<Option<String>, String> {
        if call.is_empty() || call.len() > 256 {
            return Ok(None);
        }
        let (dir, _) = self.find(id)?;
        match tool_results::newest_for(&dir, call)? {
            Some(hash) => tool_results::read(&dir, &hash),
            None => Ok(None),
        }
    }

    /// The full result a row saved with `result_sha`, whether or not the row is
    /// still in the current view. The hash names a file in this session's own
    /// results directory, and the read verifies the bytes against it.
    pub fn stored_result(&self, id: &str, sha: &str) -> Result<Option<String>, String> {
        let (dir, _) = self.find(id)?;
        tool_results::read(&dir, sha)
    }

    pub fn view(&self, id: &str) -> Result<Vec<Value>, String> {
        let (dir, _) = self.find(id)?;
        read_view(&dir).map(|(messages, _)| messages)
    }

    /// [`Self::prepare_owned`] with an admission check: `admit` sees the
    /// conversation the request would send (after the compaction it would
    /// get) before the archive, the snapshot or the transcript is written, so
    /// a refused request leaves the store exactly as it was.
    pub fn prepare_admitted<E: From<String>>(
        &self,
        id: &str,
        incoming: &[Value],
        project: Option<&Path>,
        context: PrepareContext<'_>,
        incarnation: Option<&str>,
        admit: impl FnOnce(&[Value]) -> Result<(), E>,
    ) -> Result<Prepared, PrepareFailure<E>> {
        let PrepareContext {
            snapshot,
            history,
            overhead,
            context_length,
            max_output,
            prompt_tokens,
            facts,
            provider,
            model,
            outside,
            surface,
        } = context;
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        super::session_usage::verify_at(&dir, incarnation).map_err(|error| error.message)?;
        // The binding is decided under the store lock, before a transcript,
        // archive or snapshot write, so a refusal leaves every file unchanged.
        let open = project.map(BoundProject::of);
        let mut meta_changed = false;
        match (&meta.project_path, &open) {
            (Some(bound), Some(open)) if bound != open => {
                return Err(PrepareFailure::Binding(BindingMismatch {
                    bound: bound.to_string(),
                    open: open.to_string(),
                }));
            }
            (None, Some(open)) => {
                // The first turn binds the conversation without telling the
                // model: it has no earlier path to disambiguate.
                meta.project_path = Some(open.clone());
                meta_changed = true;
            }
            _ => {}
        }
        let pending_binding = meta.pending_binding.take();
        meta_changed |= pending_binding.is_some();
        let pending_state = meta.pending_state.take();
        meta_changed |= pending_state.is_some();
        // Every turn path saves the same mark here, under the store lock and
        // in the same write as the notices: the next turn compares the project
        // against where this one started, not where an earlier one ended.
        if let Some(history) = history
            && let project = BoundProject::of(Path::new(&history.project))
            && meta.project_path.as_ref() == Some(&project)
        {
            let saved = SavedHistoryMark::for_project(project, history.at, history.max_id);
            if meta.history_mark.as_ref() != Some(&saved) {
                meta.history_mark = Some(saved);
                meta_changed = true;
            }
        }
        let (mut pending, _) = read_view(&dir)?;
        let mut filtered: Vec<Value> = incoming
            .iter()
            .filter(|message| message["role"] != "system")
            .cloned()
            .collect();
        // A window that reloaded mid-turn, or any client that died, leaves the
        // model's last round without results. Closing it here keeps the rule
        // that the store never holds a call no request may carry. A call the
        // request itself answers is not a debt, so the loop's own rounds are
        // left exactly as it sends them.
        //
        // Only the results the request appends count: a client that rebuilds
        // its history resends earlier rounds' results, and a provider that
        // numbers calls per reply reuses their ids for the open round.
        let fresh = {
            let mut probe = pending.clone();
            absorb(&mut probe, &filtered).1
        };
        let answered: Vec<String> = filtered[fresh.min(filtered.len())..]
            .iter()
            .filter_map(turns::answered_call)
            .collect();
        let answered: Vec<&str> = answered.iter().map(String::as_str).collect();
        let closed = close_unanswered_round(&dir, &mut pending, &answered);
        // What a client sends must leave a well-formed conversation, checked
        // before anything is archived or written: one orphan result would
        // refuse every later append of this conversation.
        let mut probe = pending.clone();
        absorb(&mut probe, &filtered);
        turns::check_pairing(&probe).map_err(|error| format!("sessions: {error}"))?;
        admit(&planned_request(
            &pending,
            &filtered,
            &PlanInputs {
                overhead,
                context_length,
                max_output,
                prompt_tokens,
                facts,
                id,
            },
        ))
        .map_err(PrepareFailure::Admitted)?;
        let audio_settings = crate::gui_settings::load().session_audio;
        let evicted_auditions = persist_auditions(&self.root, &mut filtered, &audio_settings)?;
        let start = pending.len();
        let (appended, absorbed) = absorb(&mut pending, &filtered);
        // The notices the model reads stand before the turn's new message, in
        // the same record, so a reload never shows the text without its cause.
        let mut notices = Vec::new();
        if let Some(pending_binding) = pending_binding
            && let Some(to) = meta.project_path.clone()
        {
            notices.push(HostNotice::ProjectBound {
                from: pending_binding.from,
                to: to.into_string(),
                cause: pending_binding.cause,
            });
        }
        if let Some(outside) = outside {
            notices.push(outside);
        }
        if let Some(surface) = surface {
            if let Some(previous) = &meta.tool_surface
                && let Some(notice) = HostNotice::tool_surface_change(previous, &surface)
            {
                notices.push(notice);
            }
            if meta.tool_surface.as_ref() != Some(&surface) {
                meta.tool_surface = Some(surface);
                meta_changed = true;
            }
        }
        let mut notice_rows = Vec::new();
        for notice in &notices {
            if let Some(row) = saved_message(&super::host_notice::notice_row(notice)) {
                notice_rows.push(row);
            }
        }
        if let Some(state) = pending_state
            && let Some(row) = saved_message(&session_state::state_row(&state))
        {
            notice_rows.push(row);
        }
        let notice_added = !notice_rows.is_empty();
        for (offset, row) in notice_rows.into_iter().enumerate() {
            pending.insert(start + offset, row);
        }
        // Archiving rewrites auditions, which can move where the request
        // joins the saved conversation, so what is written is checked again.
        // Clips archived for a refused request are collected like any orphan.
        turns::check_pairing(&pending).map_err(|error| format!("sessions: {error}"))?;
        // A version and its first transcript reference share the store lock,
        // so collection cannot remove the version between the two writes.
        versions::attach(&dir, &mut pending[start..], snapshot, project, history)?;
        let inline_audio = filtered
            .iter()
            .any(|message| message.get("input_audio").is_some());
        let closed_round = !closed.is_empty();
        let mut written = None;
        if appended > 0 || closed_round || notice_added {
            let mut added = closed;
            added.extend_from_slice(&pending[start..]);
            written = Some(self.append(&dir, &[turn_record(&now(), added)])?);
        }
        if closed_round {
            // The round is answered on disk, so its started markers are spent.
            // Cleared only after the write: a failed write keeps them, and the
            // next preparation still tells a lost result from a call never run.
            turns::spend_started(&dir, &Default::default());
        }
        let saved_auditions = pending[start..]
            .iter()
            .filter(|message| is_audition(message))
            .cloned()
            .collect();
        // Re-read: the append may have dropped older messages to stay under the
        // cap, and what is sent must be exactly what is saved.
        let (view, _) = read_view(&dir)?;
        let live = inline_audio.then(|| {
            // `absorbed` indexes the client's current-turn history; `start`
            // indexes the saved conversation. A restored session can be much
            // longer than the client tail, so these indices are not interchangeable.
            let mut merged = pending[..start].to_vec();
            merged.extend(filtered.drain(absorbed..));
            merged
        });
        let mut pending_compaction = None;
        let mut compaction_error = None;
        if !inline_audio
            && should_compact(&view, overhead, context_length, max_output, prompt_tokens)
        {
            let target = (context_length as f64 * AUTO_COMPACT_TARGET_FRACTION) as u64;
            // Planning builds the mechanical fallback and the request input
            // but writes nothing. A failed plan does not fail the turn: the
            // conversation is sent as saved and admission already bounded it.
            match plan_compaction_request(&view, target, facts, id) {
                Ok(plan) => pending_compaction = Some(plan),
                Err(error) => {
                    log::warn!("[ai] session {id} auto-compaction failed: {error}");
                    compaction_error = Some(CompactionFailure::Store);
                }
            }
        }
        if appended > 0
            || pending_compaction.is_some()
            || meta.provider != provider
            || meta.model != model
            || meta_changed
        {
            meta.provider = provider.to_owned();
            meta.model = model.to_owned();
            meta.tokens = written.map_or_else(
                || estimate_tokens(&view),
                |outcome| outcome.kept_tokens(&view),
            );
            if appended > 0 {
                meta.turns += 1;
            }
            meta.updated = now();
            write_meta(&dir, &meta)?;
        }
        Ok(Prepared {
            meta,
            messages: view,
            live,
            appended,
            compacted: None,
            pending_compaction,
            compaction_error,
            evicted_auditions,
            saved_auditions,
        })
    }

    pub fn record_reply_owned(
        &self,
        id: &str,
        reply: &Value,
        facts: &ProjectFacts,
        provider_model: (&str, &str),
        incarnation: Option<&str>,
    ) -> Result<SessionMeta, String> {
        let (provider, model) = provider_model;
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        super::session_usage::verify_at(&dir, incarnation).map_err(|error| error.message)?;
        let (mut view, _) = read_view(&dir)?;
        let appended = if let Some(message) = saved_message(reply)
            && view.last() != Some(&message)
        {
            let appended = self.append(&dir, &[turn_record(&now(), vec![message.clone()])])?;
            view.push(message);
            Some(appended)
        } else {
            None
        };
        meta.provider = provider.to_owned();
        meta.model = model.to_owned();
        meta.tokens = appended.map_or_else(
            || estimate_tokens(&view),
            |appended| appended.kept_tokens(&view),
        );
        meta.updated = now();
        let _ = facts;
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    /// A compaction a person asked for. `guard` runs under the store lock.
    ///
    /// Two windows can ask at once; both run under this lock, one after the
    /// other, and each plans from the view it reads under the lock. The second
    /// would then fold the first one's fresh summary into a summary of a
    /// summary, so a view whose newest record already is a compaction is a
    /// conflict: the conversation is in the state the request asked for.
    pub fn compact_requested(
        &self,
        id: &str,
        target_tokens: Option<u64>,
        facts: &ProjectFacts,
        guard: Guard<'_>,
    ) -> Result<Compaction, SessionError> {
        let _lock = self.mutation_lock()?;
        guard()?;
        let (dir, _) = self.find(id)?;
        let newest = read_records(&dir)?.last().map(|record| record.kind);
        if newest == Some(RecordKind::Compaction) {
            return Err(SessionError::new(
                SessionErrorKind::Conflict,
                "This conversation was just compacted; nothing new has been added since.",
            ));
        }
        Ok(self.compact_locked(id, target_tokens, facts)?)
    }

    fn compact_locked(
        &self,
        id: &str,
        target_tokens: Option<u64>,
        facts: &ProjectFacts,
    ) -> Result<Compaction, String> {
        let (dir, mut meta) = self.find(id)?;
        let (view, _) = read_view(&dir)?;
        let target = target_tokens.filter(|tokens| *tokens > 0).unwrap_or(0);
        let outcome = plan_compaction(&view, target, facts, &meta.id)?;
        // Append, never rewrite: the turns this summary replaces stay on disk so
        // the user can still read them (the panel's original view).
        self.append_compaction(&dir, &mut meta, &outcome)?;
        Ok(outcome)
    }

    /// Writes a planned compaction, or refuses when the conversation changed
    /// while the summary was being written. The model answer replaces the
    /// mechanical summary's text; `None` keeps the mechanical text.
    pub fn commit_compaction(
        &self,
        plan: CompactionPlan,
        summary: Option<&str>,
    ) -> Result<CompactionCommit, SessionError> {
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(&plan.id)?;
        let (view, _) = read_view(&dir)?;
        if view_digest(&view) != plan.view_digest {
            return Err(SessionError::new(
                SessionErrorKind::Conflict,
                "The conversation changed while its summary was being written.",
            ));
        }
        let planned = planned_messages(&plan, summary);
        let messages = planned.messages;
        if !strips_replay_state(&messages) {
            return Err(SessionError::new(
                SessionErrorKind::SessionStore,
                "Compaction produced a message that still replays provider state.",
            ));
        }
        let tokens_after = estimate_tokens(&messages);
        let outcome = Compaction {
            id: plan.id.clone(),
            compacted_at: plan.compacted_at.clone(),
            messages,
            dropped_from: plan.dropped_from,
            dropped_to: plan.dropped_to,
            tokens_before: plan.tokens_before,
            tokens_after,
            retained_pairs: plan.retained_pairs,
        };
        self.append_compaction(&dir, &mut meta, &outcome)?;
        Ok(CompactionCommit {
            compaction: outcome,
            summary_failure: planned.failure,
        })
    }

    /// Writes one compaction record and refreshes the metadata that caches it.
    /// The record is the compaction; once it landed the compaction happened, so
    /// a metadata write failure is reported in the log instead of as a failure
    /// the caller would take to mean nothing changed.
    fn append_compaction(
        &self,
        dir: &Path,
        meta: &mut SessionMeta,
        outcome: &Compaction,
    ) -> Result<(), String> {
        let record = Record {
            v: VERSION,
            kind: RecordKind::Compaction,
            at: outcome.compacted_at.clone(),
            dropped: Some(Dropped {
                from: outcome.dropped_from,
                to: outcome.dropped_to,
            }),
            tokens_before: Some(outcome.tokens_before),
            tokens_after: Some(outcome.tokens_after),
            truncated: None,
            messages: outcome.messages.clone(),
        };
        let written = self.append(dir, &[record])?;
        meta.compacted_at = Some(outcome.compacted_at.clone());
        meta.tokens = written.kept_tokens(&outcome.messages);
        meta.updated = now();
        if let Err(error) = write_meta(dir, meta) {
            log::warn!(
                "[ai] session {} compacted, but its metadata was not updated: {error}",
                outcome.id
            );
        }
        Ok(())
    }

    /// Publishes a planned compaction and owns the retry policy. A conflict
    /// means the conversation moved after the plan was built: the model text
    /// cannot be used, but one mechanical retry from the current view still
    /// compacts. Any other store failure leaves the view untouched and is
    /// reported as a store failure, never as a provider problem.
    pub fn finish_compaction(
        &self,
        plan: CompactionPlan,
        summary: Option<&str>,
    ) -> CompactionResolution {
        let id = plan.id.clone();
        let target = plan.target_tokens;
        let facts = plan.facts.clone();
        match self.commit_compaction(plan, summary) {
            Ok(commit) => CompactionResolution {
                compaction: Some(commit.compaction),
                failure: commit.summary_failure,
            },
            Err(error) if error.kind == SessionErrorKind::Conflict => {
                match self.mechanical_recompaction(&id, target, &facts) {
                    Ok(compaction) => CompactionResolution {
                        compaction,
                        failure: Some(CompactionFailure::Conflict),
                    },
                    Err(error) => {
                        log::warn!("[ai] session {id} mechanical recompaction failed: {error}");
                        CompactionResolution {
                            compaction: None,
                            failure: Some(CompactionFailure::Store),
                        }
                    }
                }
            }
            Err(error) => {
                log::warn!("[ai] session {id} compaction could not be published: {error}");
                CompactionResolution {
                    compaction: None,
                    failure: Some(CompactionFailure::Store),
                }
            }
        }
    }

    /// Plans a compaction from the conversation as it stands now, under the
    /// store lock, and writes nothing. A turn's round boundary uses this before
    /// it asks the model, so the split it commits covers the saved view.
    pub fn plan_compaction_now(
        &self,
        id: &str,
        target: u64,
        facts: &ProjectFacts,
    ) -> Result<CompactionPlan, String> {
        let _lock = self.mutation_lock()?;
        let view = self.view(id)?;
        plan_compaction_request(&view, target, facts, id)
    }

    /// Plans again from the conversation as it stands now and commits the
    /// mechanical summary. Used when a model summary's plan met a change it
    /// must not overwrite, so one retry still compacts without a second call.
    pub fn mechanical_recompaction(
        &self,
        id: &str,
        target: u64,
        facts: &ProjectFacts,
    ) -> Result<Option<Compaction>, SessionError> {
        let view = self.view(id)?;
        match plan_compaction_request(&view, target, facts, id) {
            Ok(plan) => self
                .commit_compaction(plan, None)
                .map(|commit| Some(commit.compaction)),
            Err(_) => Ok(None),
        }
    }

    /// Drops every record before a conversation's second-newest compaction and
    /// collects the assets no remaining message names. A conversation with
    /// fewer than three compactions has nothing older to drop.
    pub fn clear_old_history_guarded(
        &self,
        id: &str,
        guard: Guard<'_>,
    ) -> Result<ClearReport, SessionError> {
        let _lock = self.mutation_lock()?;
        guard()?;
        let before = root_size(&self.root);
        let (dir, meta) = self.find(id)?;
        let records = read_records(&dir)?;
        let Some((from, kept)) = old_history_from(&records) else {
            return Ok(ClearReport::default());
        };
        let dropped = records.len() - from;
        write_records(&dir, &kept)?;
        let mut meta = meta;
        meta.updated = now();
        write_meta(&dir, &meta)?;
        let removed_assets = self.collect_unreferenced_locked();
        let after = root_size(&self.root);
        Ok(ClearReport {
            sessions: 1,
            dropped_records: dropped as u64,
            removed_assets,
            freed_bytes: before.saturating_sub(after),
        })
    }

    /// [`Self::clear_old_history_guarded`] for every conversation, skipping the
    /// ones `skip` names. The sweep reads and plans every rewrite under one
    /// store lock before writing any of them, so a read or lock failure leaves
    /// the conversations it could reach unchanged.
    pub fn clear_all_old_history(
        &self,
        skip: &dyn Fn(&str) -> bool,
    ) -> Result<ClearReport, String> {
        let _lock = self.mutation_lock()?;
        let before = root_size(&self.root);
        let mut report = ClearReport::default();
        let mut keys = self.workspace_keys();
        if !keys.iter().any(|key| key == DETACHED_KEY) {
            keys.push(DETACHED_KEY.to_owned());
        }
        // Read and plan every rewrite before the first write, so a read or lock
        // failure leaves the conversations this sweep did reach untouched.
        let mut rewrites = Vec::new();
        let mut locks = Vec::new();
        for key in keys {
            let Ok(entries) = std::fs::read_dir(self.root.join(&key)) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !is_valid_id(&name) || skip(&name) {
                    continue;
                }
                // A conversation another process has open keeps its history
                // until that process closes it.
                let lock = match crate::process_lock::ProcessLock::acquire(self.lock_path(&name)) {
                    Ok(lock) => lock,
                    Err(error) => {
                        if !error.is_in_use() {
                            log::warn!("[ai] conversation {name} stays: {error}");
                        }
                        continue;
                    }
                };
                let dir = entry.path();
                let Ok(records) = read_records(&dir) else {
                    continue;
                };
                let Some((from, kept)) = old_history_from(&records) else {
                    continue;
                };
                locks.push(lock);
                rewrites.push((dir, from, records.len(), kept));
            }
        }
        // The locks stay held until every staged rewrite is published.
        let _locks = locks;
        for (dir, from, total, kept) in rewrites {
            write_records(&dir, &kept)?;
            if let Some(mut meta) = read_meta(&dir.join(META_FILE)) {
                meta.updated = now();
                let _ = write_meta(&dir, &meta);
            }
            report.sessions += 1;
            report.dropped_records += (total - from) as u64;
        }
        report.removed_assets = self.collect_unreferenced_locked();
        report.freed_bytes = before.saturating_sub(root_size(&self.root));
        Ok(report)
    }
}

// ---- autosave -----------------------------------------------------------

/// Appends the messages of one request that the saved conversation does not have
/// yet. The saved conversation is the authority: a request that extends it
/// contributes its tail, a new turn contributes its pending user message.
///
/// `incoming` carries no system messages. Returns how many messages were appended
/// and the index of the first appended one, which is where the request starts to
/// be new.
pub fn absorb(view: &mut Vec<Value>, incoming: &[Value]) -> (usize, usize) {
    if incoming.is_empty() {
        return (0, 0);
    }
    let start = match view.last() {
        None => 0,
        Some(last) => match incoming
            .iter()
            .rposition(|message| same_turn(message, last))
        {
            // A turn that continues the saved conversation: everything after the
            // last message the store already has is new.
            Some(index) => index + 1,
            // A new turn: the client rebuilt its own history, so only the
            // pending user message belongs to the saved conversation.
            None => incoming.len() - 1,
        },
    };
    let mut appended = 0;
    for message in &incoming[start..] {
        if view.last() == Some(message) {
            continue;
        }
        if let Some(message) = saved_message(message) {
            view.push(message);
            appended += 1;
        }
    }
    (appended, start)
}

pub(crate) fn audition_render_reference(message: &Value) -> Option<&str> {
    if message["role"] != "user" || !is_audition(message) {
        return None;
    }
    message["source_render"]
        .as_str()
        .filter(|path| path.chars().count() <= 4096 && Path::new(path).is_absolute())
}

/// Add replay instructions only to the provider copy of the transcript.
pub(crate) fn describe_audition_renders(root: &Path, outbound: &mut [Value]) {
    let mut paths = std::collections::HashSet::new();
    for row in outbound.iter_mut().rev() {
        let Some(path) = audition_render_reference(row) else {
            continue;
        };
        if paths.len() == super::media::MAX_ATTACHMENTS_PER_MESSAGE {
            break;
        }
        if paths.contains(path) {
            continue;
        }
        if crate::commands::saved_audition_render_path(root, Path::new(path)).is_err() {
            continue;
        }
        paths.insert(path.to_owned());
        let content = row["content"].as_str().unwrap_or_default();
        row["content"] = json!(format!(
            "{content}\nSaved arrangement render at {}. To hear another passage, call audition_clip_source with this exact path and source_start_seconds measured from the start of this render file.",
            json!(path)
        ));
    }
}

// ---- compaction ---------------------------------------------------------

/// A row the host delivered for a listening call.
fn is_audition(message: &Value) -> bool {
    message_protocol::kind(message).is_some_and(message_protocol::is_audition)
}

/// Legacy fixture helper; active readers use versioned message kinds.
#[cfg(test)]
fn is_round_nudge(content: &str) -> bool {
    message_protocol::kind(&json!({"role":"user","content":content}))
        == Some(MessageKind::RoundBudget)
}

/// Drop a trailing assistant message that carries tool calls, so a wire that
/// ends there is not replayed with a call nothing answers.
fn trim_unanswered_call(messages: &mut Vec<Value>) {
    let unanswered = |message: &Value| {
        message.get("role").and_then(Value::as_str) == Some("assistant")
            && message
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|calls| !calls.is_empty())
    };
    while messages.last().is_some_and(unanswered) {
        messages.pop();
    }
}

fn turn_record(at: &str, mut messages: Vec<Value>) -> Record {
    for message in &mut messages {
        message_protocol::tag_new(message);
    }
    Record {
        v: VERSION,
        kind: RecordKind::Turn,
        at: at.to_owned(),
        dropped: None,
        tokens_before: None,
        tokens_after: None,
        truncated: None,
        messages,
    }
}

/// Closes a last round whose calls never got a result: a window that reloaded
/// mid-turn, or a client that died, stops the loop that owed them. `answered`
/// names the calls the request being prepared brings results for, which are not
/// debts. Returns the results it added, in call order; a round that follows the
/// block is complete and is left alone.
fn close_unanswered_round(dir: &Path, view: &mut Vec<Value>, answered: &[&str]) -> Vec<Value> {
    // A call the request itself answers is not a debt.
    let mut closed = turns::close_open(view, &turns::started_in(dir, view));
    closed.retain(|message| {
        !turns::answered_call(message).is_some_and(|id| answered.contains(&id.as_str()))
    });
    view.extend(closed.iter().cloned());
    closed
}

/// The conversation a preparation would send, computed without writing:
/// the saved view plus what the request adds, compacted as the preparation
/// would compact it. An inline clip is sent from the client's own messages,
/// as the preparation sends it.
fn planned_request(pending: &[Value], incoming: &[Value], plan: &PlanInputs<'_>) -> Vec<Value> {
    let mut view = pending.to_vec();
    let (_, absorbed) = absorb(&mut view, incoming);
    if incoming
        .iter()
        .any(|message| message.get("input_audio").is_some())
    {
        let mut merged = pending.to_vec();
        merged.extend_from_slice(&incoming[absorbed.min(incoming.len())..]);
        return merged;
    }
    if should_compact(
        &view,
        plan.overhead,
        plan.context_length,
        plan.max_output,
        plan.prompt_tokens,
    ) {
        let target = (plan.context_length as f64 * AUTO_COMPACT_TARGET_FRACTION) as u64;
        if let Ok(outcome) = plan_compaction(&view, target, plan.facts, plan.id) {
            return outcome.messages;
        }
    }
    view
}

/// Versioned metadata for a host message of `kind`, as saved on the wire.
pub fn message_meta(kind: MessageKind) -> Value {
    json!(MessageMeta::new(kind))
}

// ---- message provenance -------------------------------------------------

/// The versioned kind of a saved or wire message.
pub fn message_kind(message: &Value) -> Option<MessageKind> {
    message_protocol::kind(message)
}

/// A summary the store's compaction wrote. It carries the conversation's
/// structured state (standing intent, discarded auditions); a fold row the
/// loop writes into one request carries none, so a later fold or trim can
/// keep the one and drop the other.
pub fn is_compaction_summary(message: &Value) -> bool {
    message_protocol::legacy(message).is_some_and(|meta| {
        meta.summary().is_some_and(|summary| {
            summary.standing_intent.is_some() || summary.discarded_auditions.is_some()
        })
    })
}

// ---- file I/O -----------------------------------------------------------

// ---- helpers ------------------------------------------------------------

/// The records from the `keep`-th compaction onward, counted from the oldest.
/// The compactions before it are the history this rewrite drops.
fn keep_from_compaction(records: &[Record], keep: usize) -> Option<(usize, Vec<Record>)> {
    let from = *compaction_indices(records).get(keep.checked_sub(1)?)?;
    Some((from, records[from..].to_vec()))
}

/// The rewrite that keeps only the newest two compactions and what followed
/// them. A conversation with fewer than three compactions has nothing older.
fn old_history_from(records: &[Record]) -> Option<(usize, Vec<Record>)> {
    let compactions = compaction_indices(records).len();
    (compactions >= 3)
        .then_some(compactions - 1)
        .and_then(|keep| keep_from_compaction(records, keep))
}

/// Total bytes of the files under a directory, for a before/after report.
fn root_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            match entry.metadata() {
                Ok(meta) if meta.is_dir() => pending.push(path),
                Ok(meta) => total += meta.len(),
                Err(_) => {}
            }
        }
    }
    total
}

/// The timestamp form every store records, so one file's times compare with
/// another's.
pub(crate) fn now() -> String {
    rfc3339_from(SystemTime::now())
}

fn rfc3339_from(time: SystemTime) -> String {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    oc_diag::time::format_unix_millis(millis)
}

pub(crate) fn canonical_path(path: &Path) -> PathBuf {
    crate::project_data::canonical_path(path)
}

/// The typed goal or plan change a saved row carries, if any.
pub(crate) fn saved_session_state(message: &Value) -> Option<SessionState> {
    message_protocol::state(message)
}

/// The view indexes of the newest pinned notice of every pinned kind, plus the
/// newest goal and plan rows: a fold keeps the state the model needs to read
/// later paths and the plan it is following.
pub fn pinned_notice_indices(view: &[Value]) -> Vec<usize> {
    newest_pinned_rows(view)
        .into_iter()
        .map(|(index, _)| index)
        .collect()
}

/// The newest saved row of every pinned notice kind and of the goal and plan,
/// with its view index, in the view's order.
fn newest_pinned_rows(view: &[Value]) -> Vec<(usize, Value)> {
    let mut seen_notice = std::collections::HashSet::new();
    let mut indices = Vec::new();
    for (index, message) in view.iter().enumerate().rev() {
        let Some(notice) = message_protocol::notice(message) else {
            continue;
        };
        if notice.pinned() && seen_notice.insert(notice.kind()) {
            indices.push(index);
        }
    }
    indices.extend(
        session_state::latest_state_rows(view)
            .into_iter()
            .map(|(index, _)| index),
    );
    indices.sort_unstable();
    indices
        .into_iter()
        .filter_map(|index| view.get(index).cloned().map(|row| (index, row)))
        .collect()
}

/// The digest a summary names instead of carrying the project's DSL.
pub fn dsl_digest(dsl: &str) -> String {
    let digest = Sha256::digest(dsl.as_bytes());
    let mut text = String::with_capacity(16);
    for byte in &digest[..8] {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

fn new_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let counter = ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{millis:013x}{:04x}", counter & 0xffff)
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn clean_title(text: &str) -> String {
    let text = clip(&collapse(text), MAX_TITLE_CHARS);
    if text.is_empty() {
        "New session".into()
    } else {
        text
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str, limit: usize) -> String {
    let mut clipped: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        clipped.push('…');
    }
    clipped
}

#[cfg(test)]
impl Drop for TestRootGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = TEST_ROOT.lock() {
            *slot = None;
        }
    }
}

/// A fixture store with the given project path and two recorded sessions.
#[cfg(test)]
pub(crate) fn fixture(label: &str) -> (Store, PathBuf) {
    let root = oc_test_support::temp_dir("oc-ai-sessions", label);
    let project = root.join("song.ocp");
    std::fs::write(&project, "@project\n").unwrap();
    (Store::new(root.join("sessions")), project)
}

#[cfg(test)]
mod original_tests {
    use super::*;

    #[test]
    fn the_original_view_keeps_what_a_compaction_replaced() {
        let root = std::env::temp_dir().join(format!(
            "oc-original-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::new(root.clone());
        let meta = store.create(None, None, Some("keep"), None).unwrap();
        let dir = store.find(&meta.id).unwrap().0;

        let turn = Record {
            v: 1,
            kind: RecordKind::Turn,
            at: "2026-01-01T00:00:00Z".into(),
            dropped: None,
            tokens_before: None,
            tokens_after: None,
            truncated: None,
            messages: vec![
                json!({"role":"user","content":"first"}),
                json!({"role":"assistant","content":"answer"}),
            ],
        };
        let compaction = Record {
            v: 1,
            kind: RecordKind::Compaction,
            at: "2026-01-01T00:01:00Z".into(),
            dropped: Some(Dropped { from: 0, to: 2 }),
            tokens_before: Some(10),
            tokens_after: Some(2),
            truncated: None,
            messages: vec![json!({"role":"user","content":"summary"})],
        };
        let _ = append_records(&dir, &[turn, compaction]).unwrap();

        let (current, _) = read_view(&dir).unwrap();
        assert_eq!(current.len(), 1, "the model sees the summary");
        let (original, compactions, skipped) = read_original(&dir).unwrap();
        assert_eq!(compactions, 1);
        assert_eq!(skipped, 0);
        assert_eq!(original.len(), 2, "the history keeps both turns");
        assert_eq!(original[0]["content"], "first");

        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod tests;
