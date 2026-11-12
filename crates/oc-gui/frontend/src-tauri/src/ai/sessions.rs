//! Saved AI conversations under `~/.opencompose/ai/sessions`: one directory per
//! project key, one directory per session, an append-only `messages.jsonl`.
//!
//! Layout:
//!
//! ```text
//! sessions/
//!   <project key>/project.json      the path(s) this key was derived from
//!   <project key>/<session id>/meta.json
//!   <project key>/<session id>/messages.jsonl
//!   detached/<session id>/…         a conversation with no saved project
//! ```
//!
//! The key is the first 8 bytes of the SHA-256 of the canonical project path as
//! hex, so the directory name is stable and does not leak the path. `project.json`
//! keeps the path so a project that moved can be found again.
//!
//! `messages.jsonl` holds one record per model turn. A record is appended whole
//! (`O_APPEND`, one write); the file is rewritten only when a compaction or the
//! size cap replaces the whole conversation. Limits are stated with the
//! constants below and enforced in `append`.
#[path = "session_artifacts.rs"]
mod artifacts;
#[path = "session_lookup.rs"]
pub(super) mod lookup;
#[path = "session_message.rs"]
mod message_protocol;
#[path = "session_versions.rs"]
mod versions;
use crate::private_fs;
use message_protocol::{MessageKind, MessageMeta};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const AUDITION_RENDER_STAGE: &str = ".audition-render.part";

/// Session store directory name.
pub const DIR_NAME: &str = "sessions";
/// Key for a session not bound to a project.
pub const DETACHED_KEY: &str = "detached";
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
/// One message of a turn is refused above this, so a single tool result cannot
/// consume the whole file.
pub const MAX_MESSAGE_CHARS: usize = 400_000;
/// Auto-compaction fires when the estimated request exceeds this share of the
/// model's context window...
pub const AUTO_COMPACT_FRACTION: f64 = 0.75;
/// ...but only when the conversation itself is above this share, so a large
/// system prompt alone does not compact on every turn.
pub const AUTO_COMPACT_CONVERSATION_FLOOR: f64 = 0.15;
/// The target a compaction aims for, as a share of the context window.
pub const AUTO_COMPACT_TARGET_FRACTION: f64 = 0.5;
/// Tool pairs carried into the summary as plain text.
pub const RETAINED_TOOL_PAIRS: usize = 6;
/// Marks an audition the host delivered for a listening call. The row is
/// context the model needs, never something the user said.
const CONTEXT_AUDITION: &str = "audition";
/// Marks the message that replaces compacted turns.
const CONTEXT_SUMMARY: &str = "summary";
/// The wire accepts 100 messages per request; compact before a saved session
/// gets close, so a restored conversation is always sendable.
pub const MAX_AUTO_COMPACT_MESSAGES: usize = 80;
/// Title bound; the first turn's text is the default title.
pub const MAX_TITLE_CHARS: usize = 80;
const MAX_SUMMARY_RESULT_CHARS: usize = 200;
const MAX_SUMMARY_ITEM_CHARS: usize = 160;
const MAX_INTENT_CHARS: usize = 600;

const VERSION: u32 = 1;

static ACTIVE: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);
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
    /// The project file this session last worked on. It may change: a session
    /// belongs to a workspace, not to one `.ocp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// What a call that changes the project DSL may do without asking. A
    /// session saved before this field reads as the default.
    #[serde(default)]
    pub permission: SessionPermission,
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

/// What a workspace directory records: the key it was derived from and every
/// directory path seen for it, so a moved workspace can be re-associated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorkspaceRecord {
    v: u32,
    key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    created: String,
    updated: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RecordKind {
    /// Messages appended for one model turn.
    Turn,
    /// Replaces the whole conversation with a summary.
    Compaction,
    /// Replaces the whole conversation with the newest messages that fit.
    Trim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Dropped {
    from: usize,
    to: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Record {
    v: u32,
    kind: RecordKind,
    at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dropped: Option<Dropped>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tokens_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tokens_after: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    truncated: Option<bool>,
    #[serde(default)]
    messages: Vec<Value>,
}

/// What the project looked like when a summary was written: the alternate to
/// carrying the DSL itself into the summary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectFacts {
    pub path: Option<String>,
    pub dsl_digest: String,
    pub track_count: usize,
}

/// What a listing covers: one workspace (which may be the detached one) or every
/// workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    Workspace(Option<&'a Path>),
    All,
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

/// Shared context for saving and compacting one model turn.
pub struct PrepareContext<'a> {
    /// The current document, captured with the request's project identity.
    pub snapshot: Option<&'a str>,
    pub overhead: u64,
    pub context_length: u64,
    pub facts: &'a ProjectFacts,
    pub provider: &'a str,
    pub model: &'a str,
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
    /// Archive files this turn's auditions evicted to stay inside the budget.
    pub evicted_auditions: Vec<EvictedAudition>,
    /// Audition rows added by this request, for the live operator transcript.
    pub saved_auditions: Vec<Value>,
}

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    lookup: Arc<dyn lookup::Io>,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            lookup: Arc::new(lookup::Disk),
        }
    }

    #[cfg(test)]
    pub(super) fn with_lookup(root: impl Into<PathBuf>, lookup: Arc<dyn lookup::Io>) -> Self {
        Self {
            root: root.into(),
            lookup,
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

    // ---- workspace keys -------------------------------------------------

    /// The key a workspace's conversations live under, following a recorded
    /// directory when the hash of this path has no directory yet.
    pub fn workspace_key(&self, workspace: Option<&Path>) -> String {
        let Some(path) = workspace else {
            return DETACHED_KEY.to_owned();
        };
        let key = hashed_key(path);
        if self.root.join(&key).is_dir() {
            return key;
        }
        let canonical = canonical_dir(path);
        self.recorded_key(&canonical).unwrap_or(key)
    }

    fn recorded_key(&self, canonical: &Path) -> Option<String> {
        let entries = std::fs::read_dir(&self.root).ok()?;
        for entry in entries.flatten().take(512) {
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_valid_key(&name) {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path().join(WORKSPACE_FILE)) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<WorkspaceRecord>(&bytes) else {
                continue;
            };
            if record.paths.iter().any(|path| Path::new(path) == canonical) {
                return Some(name);
            }
        }
        None
    }

    /// Re-associates a moved workspace: the directory follows the new path and
    /// the record keeps both. The record itself is written by every save, so this
    /// is the move, not the bookkeeping.
    #[cfg(test)]
    pub fn reassociate(&self, from: &Path, to: &Path) -> Result<String, String> {
        let _lock = self.mutation_lock()?;
        let from_key = hashed_key(from);
        let source = self.root.join(&from_key);
        if !source.is_dir() {
            return Err(format!("no saved conversations for {}", from.display()));
        }
        let to_key = hashed_key(to);
        let target = self.root.join(&to_key);
        if target.exists() {
            return Err(format!("conversations already exist for {}", to.display()));
        }
        private_fs::ensure_dir(&self.root).map_err(|error| format!("sessions: {error}"))?;
        std::fs::rename(&source, &target).map_err(|error| format!("sessions: {error}"))?;
        self.ensure_workspace(&to_key, Some(to), &now())?;
        Ok(to_key)
    }

    // Asset publication and the transcript that names the assets form one
    // operation from the collector's perspective.
    fn mutation_lock(&self) -> Result<std::fs::File, String> {
        self.mutation_lock_io()
            .map_err(|error| format!("Could not lock saved conversations: {error}"))
    }
    fn mutation_lock_io(&self) -> std::io::Result<std::fs::File> {
        private_fs::ensure_dir(&self.root)?;
        let lock = private_fs::lock(&self.root.join(".sessions.lock"), false, false)?;
        // Only a stopped writer can leave a stage while the lock is held.
        let _ = std::fs::remove_file(self.root.join(AUDITION_RENDER_STAGE));
        Ok(lock)
    }

    fn ensure_root(&self) -> Result<(), String> {
        private_fs::ensure_dir(&self.root).map_err(|error| format!("sessions: {error}"))
    }

    fn ensure_workspace(
        &self,
        key: &str,
        workspace: Option<&Path>,
        at: &str,
    ) -> Result<(), String> {
        self.ensure_root()?;
        let dir = self.root.join(key);
        private_fs::ensure_dir(&dir).map_err(|error| format!("sessions: {error}"))?;
        if key == DETACHED_KEY {
            return Ok(());
        }
        let file = dir.join(WORKSPACE_FILE);
        let existing = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<WorkspaceRecord>(&bytes).ok());
        let mut record = existing.unwrap_or(WorkspaceRecord {
            v: VERSION,
            key: key.to_owned(),
            workspace: None,
            paths: Vec::new(),
            created: at.to_owned(),
            updated: at.to_owned(),
        });
        if let Some(workspace) = workspace {
            let text = canonical_dir(workspace).to_string_lossy().to_string();
            record.workspace.get_or_insert_with(|| text.clone());
            if !record.paths.contains(&text) {
                record.paths.push(text);
                record.paths.truncate(8);
            }
        }
        record.updated = at.to_owned();
        let bytes = serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?;
        private_fs::write_atomic(&file, &bytes).map_err(|error| format!("sessions: {error}"))
    }

    // ---- sessions -------------------------------------------------------

    /// Sessions newest first, scoped to one workspace or to every workspace.
    pub fn list(&self, scope: Scope<'_>) -> Result<Vec<SessionMeta>, String> {
        let keys = match scope {
            Scope::Workspace(Some(path)) => vec![self.workspace_key(Some(path))],
            Scope::Workspace(None) => vec![DETACHED_KEY.to_owned()],
            Scope::All => self.workspace_keys(),
        };
        let mut sessions = Vec::new();
        for key in keys {
            let Ok(entries) = std::fs::read_dir(self.root.join(&key)) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !is_valid_id(&name) {
                    continue;
                }
                if let Some(meta) = read_meta(&entry.path().join(META_FILE)) {
                    sessions.push(meta);
                }
            }
        }
        let recorded = self.workspace_paths();
        for meta in &mut sessions {
            if let Some(path) = recorded.get(&meta.workspace_key) {
                meta.workspace = Some(path.clone());
            }
        }
        sessions.sort_by(|left, right| {
            right
                .updated
                .cmp(&left.updated)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(sessions)
    }

    fn workspace_keys(&self) -> Vec<String> {
        self.lookup.workspaces(&self.root).keys
    }

    fn workspace_paths(&self) -> HashMap<String, String> {
        let mut paths = HashMap::new();
        for key in self.workspace_keys() {
            let Ok(bytes) = std::fs::read(self.root.join(&key).join(WORKSPACE_FILE)) else {
                continue;
            };
            if let Ok(record) = serde_json::from_slice::<WorkspaceRecord>(&bytes)
                && let Some(workspace) = record.workspace
            {
                paths.insert(key, workspace);
            }
        }
        paths
    }

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
            project_path: project.map(|path| canonical_path(path).to_string_lossy().to_string()),
            workspace: workspace.map(|path| canonical_dir(path).to_string_lossy().to_string()),
            // A fork keeps the mode the conversation was working under.
            permission: parent
                .as_ref()
                .map(|parent| parent.permission)
                .unwrap_or_default(),
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

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let _lock = self.mutation_lock()?;
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
        let (mut messages, skipped) = read_view(&dir)?;
        mark_missing_media(&mut messages);
        let (usage, usage_error) =
            match self.with_usage(id, |dir| super::session_usage::snapshot_at(dir, id)) {
                Ok(usage) => (usage, None),
                Err(error) => (None, Some(error)),
            };
        Ok(Loaded {
            meta,
            usage,
            usage_error,
            messages,
            skipped,
            compacted: has_compaction(&dir),
        })
    }

    /// Drop every record before the `keep`-th compaction, then remove assets no
    /// remaining message refers to. `keep = 1` keeps only the first compaction
    /// and what followed it.
    pub fn drop_history(&self, id: &str, keep: u64) -> Result<Value, String> {
        let _lock = self.mutation_lock()?;
        if keep == 0 {
            return Err("drop_history needs a count of at least 1".into());
        }
        let (dir, mut meta) = self.find(id)?;
        let records = read_records(&dir)?;
        let compactions: Vec<usize> = records
            .iter()
            .enumerate()
            .filter(|(_, record)| record.kind == RecordKind::Compaction)
            .map(|(index, _)| index)
            .collect();
        let keep = keep as usize;
        if keep > compactions.len() {
            return Err(format!(
                "drop_history {keep}: this session has {} compaction(s)",
                compactions.len()
            ));
        }
        let from = compactions[keep - 1];
        let dropped = records.len() - from;
        let kept = records[from..].to_vec();
        write_records(&dir, &kept)?;
        meta.updated = now();
        write_meta(&dir, &meta)?;
        let removed = self.collect_unreferenced_locked();
        Ok(json!({
            "dropped_records": dropped,
            "kept_records": kept.len(),
            "removed_assets": removed,
        }))
    }

    /// Delete content-addressed assets no session message names any more:
    /// audition clips and renders shared at the store root, and each session's
    /// own DSL versions.
    #[cfg(test)]
    pub fn collect_unreferenced(&self) -> Result<u64, String> {
        let _lock = self.mutation_lock()?;
        Ok(self.collect_unreferenced_locked())
    }

    fn collect_unreferenced_locked(&self) -> u64 {
        let (referenced, version_dirs) = session_references(&self.root);
        let mut removed = 0u64;
        sweep(&clip_dir(&self.root), &referenced, &mut removed);
        sweep(&render_dir(&self.root), &referenced, &mut removed);
        for dir in version_dirs {
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

    /// Cut the conversation back to its first `keep` messages.
    ///
    /// The store is append-only, so a rewind rewrites the file with one turn
    /// that holds the kept prefix. A cut that lands inside a tool block moves
    /// back to the call that opened the block, because a provider rejects a
    /// result whose call is gone. A call left without its result is answered
    /// with a refusal, because a provider rejects a history where a call is not
    /// followed by a result.
    pub fn rewind(&self, id: &str, keep: usize) -> Result<usize, String> {
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        let (view, _) = read_view(&dir)?;
        if keep == 0 || keep > view.len() {
            return Err(format!(
                "rewind needs 1..{} messages, got {keep}",
                view.len()
            ));
        }
        let keep = block_start(&view, keep);
        if keep == 0 {
            return Err("rewind would cut before the first tool call".into());
        }
        let mut kept: Vec<Value> = view[..keep].to_vec();
        // Every result in the prefix is known before any call is judged: a
        // result always follows its call, so one pass would call each completed
        // call unanswered.
        let answered: std::collections::HashSet<&str> = kept
            .iter()
            .filter(|message| message["role"] == "tool")
            .filter_map(|message| message["tool_call_id"].as_str())
            .collect();
        let mut pending: Vec<String> = Vec::new();
        for message in &kept {
            for call in message["tool_calls"].as_array().into_iter().flatten() {
                if let Some(call) = call["id"].as_str()
                    && !answered.contains(call)
                    && !pending.iter().any(|open| open == call)
                {
                    pending.push(call.to_owned());
                }
            }
        }
        for call in pending {
            kept.push(json!({
                "role": "tool",
                "tool_call_id": call,
                "content": json!({
                    "error": "the conversation was rewound before this call returned",
                }).to_string(),
            }));
        }
        let record = Record {
            v: VERSION,
            kind: RecordKind::Turn,
            at: now(),
            dropped: None,
            tokens_before: None,
            tokens_after: None,
            truncated: None,
            messages: kept.clone(),
        };
        write_records(&dir, &[record])?;
        meta.tokens = estimate_tokens(&kept);
        meta.updated = now();
        write_meta(&dir, &meta)?;
        Ok(kept.len())
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

    pub fn view(&self, id: &str) -> Result<Vec<Value>, String> {
        let (dir, _) = self.find(id)?;
        read_view(&dir).map(|(messages, _)| messages)
    }

    /// Absorbs one request, compacts before it when the estimate says it must,
    /// and returns what should be sent. `overhead` is the estimated size of the
    /// system prompt the client rebuilds every turn.
    #[cfg(test)]
    pub fn prepare(
        &self,
        id: &str,
        incoming: &[Value],
        project: Option<&Path>,
        context: PrepareContext<'_>,
    ) -> Result<Prepared, String> {
        self.prepare_owned(id, incoming, project, context, None)
    }

    /// Admits transcript preparation against the same saved-session incarnation
    /// as the request's accounting owner, before any transcript or archive write.
    pub fn prepare_owned(
        &self,
        id: &str,
        incoming: &[Value],
        project: Option<&Path>,
        context: PrepareContext<'_>,
        incarnation: Option<&str>,
    ) -> Result<Prepared, String> {
        let PrepareContext {
            snapshot,
            overhead,
            context_length,
            facts,
            provider,
            model,
        } = context;
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        super::session_usage::verify_at(&dir, incarnation).map_err(|error| error.message)?;
        let (mut pending, _) = read_view(&dir)?;
        let mut filtered: Vec<Value> = incoming
            .iter()
            .filter(|message| message["role"] != "system")
            .cloned()
            .collect();
        let audio_settings = crate::gui_settings::load().session_audio;
        let evicted_auditions = persist_auditions(&self.root, &mut filtered, &audio_settings)?;
        // A window that reloaded mid-turn, or any client that died, leaves the
        // model's last round without results. Closing it here keeps the rule
        // that the store never holds a call no request may carry. A call the
        // request itself answers is not a debt, so the loop's own rounds are
        // left exactly as it sends them.
        let answered: Vec<&str> = filtered
            .iter()
            .filter_map(|message| message["tool_call_id"].as_str())
            .collect();
        let closed = close_unanswered_round(&mut pending, &answered);
        let start = pending.len();
        let (appended, absorbed) = absorb(&mut pending, &filtered);
        // A version and its first transcript reference share the store lock,
        // so collection cannot remove the version between the two writes.
        versions::attach(&dir, &mut pending[start..], snapshot)?;
        let inline_audio = filtered
            .iter()
            .any(|message| message.get("input_audio").is_some());
        if appended > 0 || !closed.is_empty() {
            let mut added = closed;
            added.extend_from_slice(&pending[start..]);
            append_records(&dir, &[turn_record(&now(), added)])?;
        }
        let saved_auditions = pending[start..]
            .iter()
            .filter(|message| is_audition(message))
            .cloned()
            .collect();
        // A session belongs to a workspace, not to one file: opening another
        // `.ocp` in the same workspace keeps the conversation and records the file.
        if let Some(project) = project {
            let path = canonical_path(project).to_string_lossy().to_string();
            if meta.project_path.as_deref() != Some(path.as_str()) {
                meta.project_path = Some(path);
            }
        }
        // Re-read: the append may have dropped older messages to stay under the
        // cap, and what is sent must be exactly what is saved.
        let (mut view, _) = read_view(&dir)?;
        let live = inline_audio.then(|| {
            // `absorbed` indexes the client's current-turn history; `start`
            // indexes the saved conversation. A restored session can be much
            // longer than the client tail, so these indices are not interchangeable.
            let mut merged = pending[..start].to_vec();
            merged.extend(filtered.drain(absorbed..));
            merged
        });
        let mut compacted = None;
        if !inline_audio && should_compact(&view, overhead, context_length) {
            let target = (context_length as f64 * AUTO_COMPACT_TARGET_FRACTION) as u64;
            if let Ok(outcome) = self.compact_locked(id, Some(target), facts) {
                view = outcome.messages.clone();
                meta.compacted_at = Some(outcome.compacted_at.clone());
                meta.tokens = outcome.tokens_after;
                compacted = Some(outcome);
            }
        }
        if appended > 0 || compacted.is_some() || meta.provider != provider || meta.model != model {
            meta.provider = provider.to_owned();
            meta.model = model.to_owned();
            meta.tokens = estimate_tokens(&view);
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
            compacted,
            evicted_auditions,
            saved_auditions,
        })
    }

    /// Saves locally completed tool results before returning control to the user.
    /// Only outstanding saved call IDs are accepted; repeated checkpoints are idempotent.
    pub fn checkpoint_tools(
        &self,
        id: &str,
        results: &[Value],
        outcome: Option<(&str, &str)>,
    ) -> Result<(), String> {
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        let (view, _) = read_view(&dir)?;
        let mut pending = std::collections::HashSet::new();
        for message in &view {
            for call in message["tool_calls"].as_array().into_iter().flatten() {
                if let Some(id) = call["id"].as_str() {
                    pending.insert(id.to_owned());
                }
            }
            if message["role"] == "tool"
                && let Some(id) = message["tool_call_id"].as_str()
            {
                pending.remove(id);
            }
        }
        let mut added = Vec::new();
        for message in results {
            if message["role"] == "tool"
                && message["tool_call_id"]
                    .as_str()
                    .is_some_and(|id| pending.remove(id))
                && let Some(saved) = saved_message(message)
            {
                added.push(saved);
            }
        }
        if let Some((status, path)) = outcome {
            if !matches!(status, "applied" | "rejected") || path.len() > 1024 {
                return Err("Invalid proposal outcome".into());
            }
            // Answer the call that proposed it: a provider requires the tool
            // result to follow its call immediately, and a user message in
            // between makes the next request invalid. With no pending call the
            // outcome stays a note.
            let body = format!(
                "Host proposal outcome: {}",
                json!({"status":status,"path":path,"other_deferred_calls_applied":false})
            );
            let mut message = match pending.iter().next() {
                Some(call) => json!({
                    "role": "tool",
                    "tool_call_id": call,
                    "content": body,
                }),
                None => json!({"role": "user", "content": body}),
            };
            message_protocol::tag(&mut message, MessageMeta::new(MessageKind::ProposalOutcome));
            if view.last() != Some(&message) {
                added.push(message);
            }
        }
        if !added.is_empty() {
            append_records(&dir, &[turn_record(&now(), added)])?;
            let (view, _) = read_view(&dir)?;
            meta.tokens = estimate_tokens(&view);
            meta.updated = now();
            write_meta(&dir, &meta)?;
        }
        Ok(())
    }

    /// Records a host transaction outcome independently of the panel's stale wire history.
    pub fn record_rollback(&self, id: &str, facts: &ProjectFacts) -> Result<(), String> {
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        let (mut view, _) = read_view(&dir)?;
        let mut messages = close_unanswered_round(&mut view, &[]);
        let mut rollback = json!({"role": "user", "content": format!(
            "Host transaction rollback: all project edits in the interrupted transaction were reverted. Earlier successful mutation results describe historical intermediate states, not the current project. Current project DSL sha256 {}; track count {}. Inspect current project before further edits. External rendered or separated audio files are not reverted.",
            facts.dsl_digest, facts.track_count
        )});
        message_protocol::tag(
            &mut rollback,
            MessageMeta::new(MessageKind::TransactionRollback),
        );
        messages.push(rollback);
        append_records(&dir, &[turn_record(&now(), messages)])?;
        let (view, _) = read_view(&dir)?;
        meta.tokens = estimate_tokens(&view);
        meta.updated = now();
        write_meta(&dir, &meta)
    }

    /// Appends the assistant message a turn produced, so an answer survives the
    /// panel closing before the next turn.
    #[cfg(test)]
    pub fn record_reply(
        &self,
        id: &str,
        reply: &Value,
        facts: &ProjectFacts,
        provider: &str,
        model: &str,
    ) -> Result<SessionMeta, String> {
        self.record_reply_owned(id, reply, facts, (provider, model), None)
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
        if let Some(message) = saved_message(reply)
            && view.last() != Some(&message)
        {
            append_records(&dir, &[turn_record(&now(), vec![message.clone()])])?;
            view.push(message);
        }
        meta.provider = provider.to_owned();
        meta.model = model.to_owned();
        meta.tokens = estimate_tokens(&view);
        meta.updated = now();
        let _ = facts;
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    /// Folds the older turns into one plain-text message that also carries the
    /// most recent tool pairs as text. No assistant tool call, provider state or
    /// signature survives.
    pub fn compact(
        &self,
        id: &str,
        target_tokens: Option<u64>,
        facts: &ProjectFacts,
    ) -> Result<Compaction, String> {
        let _lock = self.mutation_lock()?;
        self.compact_locked(id, target_tokens, facts)
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
        let at = outcome.compacted_at.clone();
        let record = Record {
            v: VERSION,
            kind: RecordKind::Compaction,
            at: at.clone(),
            dropped: Some(Dropped {
                from: outcome.dropped_from,
                to: outcome.dropped_to,
            }),
            tokens_before: Some(outcome.tokens_before),
            tokens_after: Some(outcome.tokens_after),
            truncated: None,
            messages: outcome.messages.clone(),
        };
        // Append, never rewrite: the turns this summary replaces stay on disk so
        // the user can still read them (the panel's original view).
        append_records(&dir, &[record])?;
        meta.compacted_at = Some(at);
        meta.tokens = outcome.tokens_after;
        meta.updated = now();
        write_meta(&dir, &meta)?;
        Ok(outcome)
    }
}

/// Content-addressed assets outlive individual sessions so forks retain valid
/// references. A full archive makes room by evicting its oldest files; a file a
/// live conversation names is evicted only after every unreferenced one.
fn persist_auditions(
    root: &Path,
    messages: &mut [Value],
    settings: &crate::gui_settings::SessionAudioSettings,
) -> Result<Vec<EvictedAudition>, String> {
    use base64::Engine as _;
    let mut archive = ArchiveSweep::new(root);
    for message in messages {
        let Some(audio) = message.get("input_audio") else {
            continue;
        };
        super::media::inline_audio(audio).map_err(|error| error.message)?;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(audio["data"].as_str().unwrap_or_default())
            .map_err(|_| "Invalid audition audio")?;
        // The clip is the audio the model already listened to. Archive those
        // bytes and point the wire at them, so a later request replays the same
        // duration, channel count and rate. The render the clip was cut from can
        // be longer, wider and larger than one message may carry, so it stays
        // out of the delivery.
        let clip_path = archive.clip(&raw)?;
        let clip = super::media::attach(&clip_path.to_string_lossy())?;
        let descriptor = super::media::to_wire(clip.kind, &clip.media);
        // Keep the render by path. A later audition may request another
        // passage, but normal replay sends only the bounded delivery.
        let mut render_unavailable = false;
        let render = audio
            .get("source_path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .and_then(|source| {
                let result = archive.render(&source, settings);
                crate::commands::audition_scratch::finish_handoff(&source);
                match result {
                    Ok(path) if path != clip_path => Some(path),
                    Ok(_) => None,
                    Err(error) => {
                        log::warn!("audition render not archived: {error}");
                        render_unavailable = true;
                        None
                    }
                }
            });
        message_protocol::tag_new(message);
        let host_audition =
            message_protocol::kind(message).is_some_and(message_protocol::is_audition);
        let object = message.as_object_mut().ok_or("Invalid audition message")?;
        object.remove("input_audio");
        // The delivery is host context, not something the user typed: the
        // transcript and later compactions read this marker, not the label.
        if host_audition {
            object.insert("context".into(), json!(CONTEXT_AUDITION));
        }
        if let Some(render) = render {
            object.insert("source_render".into(), json!(render.to_string_lossy()));
        }
        if render_unavailable {
            object.insert("source_render_unavailable".into(), json!(true));
        }
        let media = object
            .entry("media")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("Invalid audition attachments")?;
        media.push(descriptor);
    }
    Ok(archive.evicted)
}

/// The audition archive's cap, shared by its two directories.
pub const AUDITION_ARCHIVE_BUDGET: u64 = 256 * 1024 * 1024;

/// One directory of the audition archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditionDir {
    pub dir: String,
    pub files: usize,
    pub bytes: u64,
}

/// What the audition archive holds. The budget covers both directories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditionArchive {
    pub media: AuditionDir,
    pub renders: AuditionDir,
    pub budget_bytes: u64,
}

/// A file the archive removed to stay inside its budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvictedAudition {
    /// The directory's name: `audition-media` or `audition-renders`.
    pub dir: String,
    pub file: String,
    pub bytes: u64,
}

/// Delivery clips: the bounded audio a request inlines, keyed by content.
fn clip_dir(root: &Path) -> PathBuf {
    root.join("audition-media")
}

/// Full-rate renders kept for the operator. No request reads this directory.
fn render_dir(root: &Path) -> PathBuf {
    root.join("audition-renders")
}

/// The archive as the Storage page reports it.
pub fn audition_archive(root: &Path) -> AuditionArchive {
    AuditionArchive {
        media: audition_dir(&clip_dir(root)),
        renders: audition_dir(&render_dir(root)),
        budget_bytes: AUDITION_ARCHIVE_BUDGET,
    }
}

fn audition_dir(dir: &Path) -> AuditionDir {
    let (files, bytes) = dir_usage(dir);
    AuditionDir {
        dir: dir.display().to_string(),
        files,
        bytes,
    }
}

fn dir_usage(dir: &Path) -> (usize, u64) {
    let mut files = 0;
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        files += 1;
        bytes += meta.len();
    }
    (files, bytes)
}

/// Every hash a saved conversation names, and each session's version folder.
fn session_references(root: &Path) -> (std::collections::HashSet<String>, Vec<PathBuf>) {
    let mut referenced = std::collections::HashSet::new();
    let mut version_dirs = Vec::new();
    for workspace in read_dirs(root) {
        for session in read_dirs(&workspace) {
            version_dirs.push(session.join("versions"));
            let Ok(text) = std::fs::read_to_string(session.join(MESSAGES_FILE)) else {
                continue;
            };
            for token in text.split(|c: char| !c.is_ascii_hexdigit()) {
                if token.len() == 64 {
                    referenced.insert(token.to_ascii_lowercase());
                }
            }
        }
    }
    (referenced, version_dirs)
}

fn referenced_tokens(root: &Path) -> std::collections::HashSet<String> {
    session_references(root).0
}

/// The directories under `root`, so the archive beside the sessions is never
/// read as a workspace.
fn read_dirs(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "audition-media" || name == "audition-renders")
        })
        .collect()
}

/// Delete every file in `dir` whose content hash no conversation names.
fn sweep(dir: &Path, referenced: &std::collections::HashSet<String>, removed: &mut u64) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if referenced.contains(&stem) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            *removed += 1;
        }
    }
}

/// One write into the shared archive: the hashes conversations name, the files
/// this write already produced, and what it had to remove for room.
struct ArchiveSweep<'a> {
    root: &'a Path,
    referenced: Option<std::collections::HashSet<String>>,
    protected: std::collections::HashSet<String>,
    evicted: Vec<EvictedAudition>,
}

impl<'a> ArchiveSweep<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            referenced: None,
            protected: std::collections::HashSet::new(),
            evicted: Vec::new(),
        }
    }

    /// Reads the conversations once, and only when the archive is actually over
    /// its budget.
    fn referenced(&mut self) -> std::collections::HashSet<String> {
        self.referenced
            .get_or_insert_with(|| referenced_tokens(self.root))
            .clone()
    }

    /// Makes room for `add` bytes, evicting the archive's oldest files.
    fn room(&mut self, add: u64) -> Result<(), String> {
        let used = archived_bytes(self.root);
        if used.saturating_add(add) <= AUDITION_ARCHIVE_BUDGET {
            return Ok(());
        }
        if add > AUDITION_ARCHIVE_BUDGET {
            return Err(format!(
                "one audition file of {add} bytes is larger than the {} MiB archive",
                AUDITION_ARCHIVE_BUDGET / (1024 * 1024)
            ));
        }
        let referenced = self.referenced();
        let evicted = make_room(self.root, add, &referenced, &self.protected);
        let freed: u64 = evicted.iter().map(|entry| entry.bytes).sum();
        self.evicted.extend(evicted);
        if used.saturating_add(add).saturating_sub(freed) > AUDITION_ARCHIVE_BUDGET {
            return Err("The audition archive is full of files this turn is still writing".into());
        }
        Ok(())
    }

    /// Keep the bounded model delivery byte-for-byte so later turns hear
    /// the same sample rate, channels and duration.
    fn clip(&mut self, raw: &[u8]) -> Result<PathBuf, String> {
        let hash = format!("{:x}", Sha256::digest(raw));
        let path = clip_dir(self.root).join(format!("{hash}.wav"));
        self.protected.insert(hash);
        if !path.is_file() {
            self.room(raw.len() as u64)?;
            private_fs::write_atomic(&path, raw)
                .map_err(|error| format!("Could not save audition audio: {error}"))?;
        }
        Ok(path)
    }

    /// Archive a rendered file at its own rate and channel count, so a stereo
    /// render stays stereo. Falls back to the clip's bytes when the container
    /// cannot hold the file's rate.
    fn render(
        &mut self,
        source: &Path,
        settings: &crate::gui_settings::SessionAudioSettings,
    ) -> Result<PathBuf, String> {
        let _scratch = crate::commands::audition_scratch::pin_reader(source)?;
        let media_dir = render_dir(self.root);
        let mut input = std::fs::File::open(source)
            .map_err(|error| format!("Could not open the render: {error}"))?;
        let length = input
            .metadata()
            .map_err(|error| format!("Could not measure the render: {error}"))?
            .len();
        if length > AUDITION_ARCHIVE_BUDGET {
            return Err("The full render exceeds the audition archive budget".into());
        }
        let hash = read_render_bytes(&mut input, &mut std::io::sink(), length)
            .map_err(|error| format!("Could not read the render: {error}"))?;
        let write_original = |input: &mut std::fs::File, path: &Path| {
            private_fs::write_atomic_with(path, |file| {
                if read_render_bytes(input, file, length)? != hash {
                    return Err(std::io::Error::other("The render changed while saving"));
                }
                Ok(())
            })
            .map_err(|error| format!("Could not save audition render: {error}"))
        };
        // Use another handle to the pinned file, including when the path is
        // replaced while the archive is being written.
        let open_wav = |input: &std::fs::File| oc_engine::wav::open_file_reader(input.try_clone()?);
        let archive_key =
            |profile: &str| format!("{:x}", Sha256::digest(format!("{hash}:{profile}")));
        let verify_source = || {
            let source_hash = std::fs::File::open(source)
                .and_then(|mut file| read_render_bytes(&mut file, &mut std::io::sink(), length))
                .map_err(|error| format!("Could not verify the render: {error}"))?;
            if source_hash != hash {
                return Err("The render changed while saving".into());
            }
            Ok::<(), String>(())
        };
        match settings.format {
            crate::gui_settings::SessionAudioFormat::Wav => {
                let key = archive_key("wav-44100");
                let path = media_dir.join(format!("{key}.wav"));
                self.protected.insert(key);
                if !path.is_file() {
                    // The archive is a standard 44.1 kHz stereo file a DAW can
                    // open, independent of the rate the model's clip is shaped to.
                    if let Ok(wav) = open_wav(&input) {
                        let spec = wav.spec();
                        if spec.sample_rate == 44_100 {
                            drop(wav);
                            self.room(length)?;
                            write_original(&mut input, &path)?;
                            return Ok(path);
                        }
                        if let Some(mut conversion) =
                            oc_engine::wav::PcmWavConversion::new(wav, 44_100)
                        {
                            self.room(conversion.encoded_len())?;
                            let saved = private_fs::write_atomic_with(&path, |file| {
                                conversion.write(file)?;
                                drop(conversion);
                                if read_render_bytes(&mut input, &mut std::io::sink(), length)?
                                    != hash
                                {
                                    return Err(std::io::Error::other(
                                        "The render changed while saving",
                                    ));
                                }
                                Ok(())
                            });
                            match saved {
                                Ok(()) => return Ok(path),
                                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {}
                                Err(error) => {
                                    return Err(format!("Could not save audition audio: {error}"));
                                }
                            }
                        }
                    }
                    let decoded = oc_engine::audio_clip::decode(source)
                        .ok()
                        .filter(|audio| audio.sample_rate != 44_100);
                    if let Some(decoded) = decoded {
                        let resampled = oc_engine::resample::resample(
                            &decoded.samples,
                            decoded.sample_rate,
                            44_100,
                            oc_engine::resample::ResampleQuality::DEFAULT_RENDER,
                        )?;
                        let wav = oc_engine::wav::pcm_wav_bytes(&resampled, 44_100, 2)?;
                        verify_source()?;
                        self.room(wav.len() as u64)?;
                        private_fs::write_atomic(&path, &wav)
                            .map_err(|error| format!("Could not save audition audio: {error}"))?;
                    } else {
                        self.room(length)?;
                        write_original(&mut input, &path)?;
                    }
                }
                Ok(path)
            }
            crate::gui_settings::SessionAudioFormat::Mp3 => {
                let wav = open_wav(&input).ok();
                let profile = match &wav {
                    Some(wav) => {
                        let rate = archive_mp3_rate(wav.spec().sample_rate);
                        let bitrate = mp3_bitrate(rate, settings.mp3_kbps);
                        format!("mp3-cbr-{rate}-{bitrate}")
                    }
                    None => format!("mp3-cbr-requested-{}", settings.mp3_kbps),
                };
                let key = archive_key(&profile);
                let path = media_dir.join(format!("{key}.mp3"));
                self.protected.insert(key);
                if path.is_file() {
                    return Ok(path);
                }
                let mp3_spec = |rate| oc_engine::encode::EncodeSpec {
                    container: oc_dsl::ast::ExportContainer::Mp3,
                    wav: oc_engine::encode::WavOptions::default(),
                    flac: oc_engine::encode::FlacOptions::default(),
                    mp3: oc_engine::encode::Mp3Options {
                        bitrate: mp3_bitrate(rate, settings.mp3_kbps),
                        vbr_quality: None,
                    },
                    sample_rate: rate,
                    channels: 2,
                };
                private_fs::ensure_dir(&media_dir).map_err(|error| {
                    format!("Could not create the audition render archive: {error}")
                })?;
                // Keep incomplete encodes out of the archive and account for
                // the encoded size before making the final name visible.
                let stage = self.root.join(AUDITION_RENDER_STAGE);
                private_fs::write_atomic(&stage, &[])
                    .map_err(|error| format!("Could not stage the audition render: {error}"))?;
                let encoded = (|| {
                    if let Some(wav) = wav {
                        let rate = wav.spec().sample_rate;
                        if archive_mp3_rate(rate) == rate {
                            return oc_engine::encode::mp3::transcode_at_source_rate(
                                source,
                                &stage,
                                &mp3_spec(rate),
                            );
                        }
                        if let Some(mut conversion) =
                            oc_engine::wav::PcmWavConversion::new(wav, 48_000)
                        {
                            return oc_engine::encode::mp3::transcode_resampled_wav(
                                source,
                                &mut conversion,
                                &stage,
                                &mp3_spec(48_000),
                            );
                        }
                    }
                    let audio = oc_engine::audio_clip::decode(source)?;
                    if archive_mp3_rate(audio.sample_rate) == audio.sample_rate {
                        return oc_engine::encode::encode_to_file(
                            &stage,
                            &mp3_spec(audio.sample_rate),
                            &audio.samples,
                        )
                        .map(|_| ());
                    }
                    let resampled = oc_engine::resample::resample(
                        &audio.samples,
                        audio.sample_rate,
                        48_000,
                        oc_engine::resample::ResampleQuality::DEFAULT_RENDER,
                    )?;
                    oc_engine::encode::encode_to_file(&stage, &mp3_spec(48_000), &resampled)
                        .map(|_| ())
                })();
                if let Err(error) = encoded {
                    log::warn!("Could not encode the audition render as MP3: {error}");
                    let _ = std::fs::remove_file(&stage);
                    let key = archive_key("original-fallback");
                    let wav = media_dir.join(format!("{key}.wav"));
                    self.protected.insert(key);
                    if !wav.is_file() {
                        self.room(length)?;
                        write_original(&mut input, &wav)?;
                    }
                    return Ok(wav);
                }
                let published = (|| {
                    verify_source()?;
                    let length = std::fs::metadata(&stage)
                        .map_err(|error| format!("Could not measure the archived MP3: {error}"))?
                        .len();
                    self.room(length)?;
                    std::fs::rename(&stage, &path)
                        .map_err(|error| format!("Could not publish the archived MP3: {error}"))
                })();
                if published.is_err() {
                    let _ = std::fs::remove_file(&stage);
                }
                published?;
                Ok(path)
            }
        }
    }
}

/// Hash or copy the same bounded source without materializing the full render.
fn read_render_bytes(
    input: &mut std::fs::File,
    output: &mut impl std::io::Write,
    length: u64,
) -> std::io::Result<String> {
    use std::io::{Read, Seek};
    input.rewind()?;
    let mut hash = Sha256::new();
    let mut remaining = length;
    let mut buffer = [0; 64 * 1024];
    while remaining > 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..count])?;
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    if input.read(&mut buffer[..1])? != 0 {
        return Err(std::io::Error::other("The render grew while saving"));
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn archived_bytes(root: &Path) -> u64 {
    dir_usage(&clip_dir(root)).1 + dir_usage(&render_dir(root)).1
}

/// Removes the oldest files the archive may give up until `add` more bytes fit.
///
/// Unreferenced files go first, oldest first. A referenced render goes before a
/// referenced clip, because a clip is what a later request replays; a render is
/// the operator's copy, and a message that names one keeps the path after the
/// file is gone. Never removes a file this write produced.
fn make_room(
    root: &Path,
    add: u64,
    referenced: &std::collections::HashSet<String>,
    protected: &std::collections::HashSet<String>,
) -> Vec<EvictedAudition> {
    let mut used = archived_bytes(root);
    if used.saturating_add(add) <= AUDITION_ARCHIVE_BUDGET {
        return Vec::new();
    }
    let mut candidates: Vec<(bool, bool, SystemTime, String, String, u64)> = Vec::new();
    for (dir, is_clip) in [(clip_dir(root), true), (render_dir(root), false)] {
        let label = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if protected.contains(&stem) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let referenced = referenced.contains(&stem);
            candidates.push((
                referenced,
                is_clip && referenced,
                meta.modified().unwrap_or(UNIX_EPOCH),
                label.clone(),
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string(),
                meta.len(),
            ));
        }
    }
    candidates.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
            .then(left.3.cmp(&right.3))
            .then(left.4.cmp(&right.4))
    });
    let mut evicted = Vec::new();
    for (_, _, _, dir, file, bytes) in candidates {
        if used.saturating_add(add) <= AUDITION_ARCHIVE_BUDGET {
            break;
        }
        if std::fs::remove_file(root.join(&dir).join(&file)).is_ok() {
            used = used.saturating_sub(bytes);
            evicted.push(EvictedAudition { dir, file, bytes });
        }
    }
    evicted
}

/// Keep a supported source rate, or use the archive's resampling rate.
fn archive_mp3_rate(source_rate: u32) -> u32 {
    if oc_engine::encode::mp3::Mp3Version::for_sample_rate(source_rate).is_some() {
        source_rate
    } else {
        48_000
    }
}

/// Round a stored preference down to a CBR value the encoder accepts.
fn mp3_bitrate(rate: u32, wanted: u32) -> u32 {
    let table = oc_engine::encode::mp3::Mp3Version::for_sample_rate(rate)
        .expect("archive MP3 rate is supported")
        .bitrates();
    table
        .iter()
        .rev()
        .copied()
        .find(|&bitrate| bitrate <= wanted)
        .unwrap_or(table[0])
}

/// Bound replay only, never the stored conversation. The newest explicit user
/// attachments win; older saved clips stay on disk and are named as omitted.
pub fn bound_media_replay(outbound: &mut [Value], incoming: &[Value]) {
    let newest = incoming
        .iter()
        .rev()
        .find(|message| message["role"] == "user");
    let mut required = std::collections::HashSet::new();
    if let Some(message) = incoming.iter().rev().find(|message| {
        message["role"] == "user"
            && message["media"]
                .as_array()
                .is_some_and(|media| !media.is_empty())
    }) && let Some(media) = message["media"].as_array()
    {
        for item in media {
            required.insert(item["sha256"].as_str().unwrap_or_default().to_owned());
        }
    }
    if let Some(message) = newest
        && let Some(data) = message["input_audio"]["data"].as_str()
    {
        use base64::Engine as _;
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
            required.insert(format!("{:x}", Sha256::digest(bytes)));
        }
    }
    let cost = |entry: &Value| {
        if entry["local_reference"].as_bool() == Some(true) {
            return 0;
        }
        entry["bytes"].as_u64().unwrap_or(0).saturating_add(2) / 3 * 4
    };
    let mut used = outbound
        .iter()
        .filter_map(|m| m["media"].as_array())
        .flatten()
        .filter(|entry| required.contains(entry["sha256"].as_str().unwrap_or_default()))
        .map(cost)
        .sum::<u64>();
    let mut omitted = Vec::new();
    let mut omitted_count = 0;
    for message in outbound.iter_mut().rev() {
        let Some(media) = message.get_mut("media").and_then(Value::as_array_mut) else {
            continue;
        };
        media.retain(|entry| {
            if entry["local_reference"].as_bool() == Some(true)
                || required.contains(entry["sha256"].as_str().unwrap_or_default())
            {
                return true;
            }
            let size = cost(entry);
            if used.saturating_add(size) <= super::media::MAX_REQUEST_INLINE_BYTES as u64 {
                used += size;
                true
            } else {
                omitted_count += 1;
                if omitted.len() < 8 {
                    omitted.push(format!(
                        "{} at {}",
                        clip(entry["name"].as_str().unwrap_or("audio"), 100),
                        clip(entry["path"].as_str().unwrap_or(""), 300)
                    ));
                }
                false
            }
        });
    }
    if omitted_count > 0
        && let Some(message) = outbound
            .iter_mut()
            .rev()
            .find(|message| message["role"] == "user")
    {
        let content = message["content"].as_str().unwrap_or_default();
        message["content"] = json!(format!(
            "{content}\n[Audio replay budget: {omitted_count} older attachment(s) are saved on disk but NOT included in this request. Do not claim to have heard them. Request reattachment of a listed file when needed. Up to 8 omitted files: {}]",
            omitted.join("; ")
        ));
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

/// Matches normalized prose and tool identity while ignoring provider metadata.
fn same_turn(left: &Value, right: &Value) -> bool {
    let calls = |message: &Value| -> Vec<Value> {
        message["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|call| {
                json!([
                    call["id"],
                    call["function"]["name"],
                    call["function"]["arguments"]
                ])
            })
            .collect()
    };
    left["role"] == right["role"]
        && message_protocol::for_new(left).map(|meta| meta.kind)
            == message_protocol::for_new(right).map(|meta| meta.kind)
        && left["content"].as_str().unwrap_or_default()
            == right["content"].as_str().unwrap_or_default()
        && left["tool_call_id"] == right["tool_call_id"]
        && calls(left) == calls(right)
        && left["source_render"] == right["source_render"]
        && canonical_media(&left["media"]) == canonical_media(&right["media"])
        && (!(left.get("message_meta").is_some() && message_protocol::read(left).is_none()
            || right.get("message_meta").is_some() && message_protocol::read(right).is_none())
            || left["message_meta"] == right["message_meta"])
}

/// The saved form of a wire message: role, text and tool envelope, with an
/// inline audio clip replaced by a note (its bytes are not saved) and oversized
/// text clipped.
fn saved_message(message: &Value) -> Option<Value> {
    let role = message["role"].as_str()?;
    if role != "user" && role != "assistant" && role != "tool" {
        return None;
    }
    let mut saved = serde_json::Map::new();
    saved.insert("role".into(), json!(role));
    if let Some(meta) = message.get("message_meta") {
        saved.insert("message_meta".into(), meta.clone());
    } else if let Some(meta) = message_protocol::for_new(message) {
        saved.insert("message_meta".into(), json!(meta));
    }
    let mut content = message["content"].as_str().unwrap_or_default().to_owned();
    if content.chars().count() > MAX_MESSAGE_CHARS {
        content = clip(&content, MAX_MESSAGE_CHARS);
    }
    if message.get("input_audio").is_some() {
        content.push_str("\n[audio clip not saved]");
    }
    if let Some(media) = message.get("media").filter(|media| media.is_array()) {
        let media = canonical_media(media);
        if media.as_array().is_some_and(|entries| !entries.is_empty()) {
            saved.insert("media".into(), media);
        }
    }
    // The archive is a path rather than a media descriptor, so regular
    // provider replay never reads the whole render into the request.
    if let Some(render) = message
        .get("source_render")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty() && path.chars().count() <= 4096)
    {
        saved.insert("source_render".into(), json!(render));
    }
    if message["source_render_unavailable"].as_bool() == Some(true) {
        saved.insert("source_render_unavailable".into(), json!(true));
    }
    saved.insert("content".into(), json!(content));
    if let Some(context) = message
        .get("context")
        .filter(|value| is_context_marker(value))
    {
        saved.insert("context".into(), context.clone());
    }
    // The snapshot a message was sent against. Restoring the document from the
    // row needs this after a reload, and the sweep treats it as a reference to
    // the version file, so dropping it here deletes that file.
    if let Some(sha) = message["dsl_sha"]
        .as_str()
        .filter(|sha| sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit()))
    {
        saved.insert("dsl_sha".into(), json!(sha));
    }
    // The signed reasoning of an assistant turn is provider state: a restored
    // conversation that dropped it would replay an unsigned tool round.
    for key in ["tool_calls", "tool_call_id", "provider_state", "thinking"] {
        if let Some(value) = message.get(key) {
            saved.insert(key.into(), value.clone());
        }
    }
    Some(Value::Object(saved))
}

/// The stored form of one message's attachments: canonical descriptors,
/// with a malformed entry dropped rather than failing the whole turn.
fn canonical_media(media: &Value) -> Value {
    let entries = media
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| super::media::from_saved(entry).ok())
                .map(|(kind, reference)| super::media::to_wire(kind, &reference))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Value::Array(entries)
}

/// Rewrites every saved attachment into the canonical shape on read, so a file
/// an older build wrote cannot fail a turn or hide a restored attachment.
fn canonicalize_media(messages: &mut [Value]) {
    for message in messages {
        let canonical = match message.get("media").filter(|media| media.is_array()) {
            Some(media) => canonical_media(media),
            None => continue,
        };
        if canonical
            .as_array()
            .is_some_and(|entries| entries.is_empty())
        {
            if let Some(object) = message.as_object_mut() {
                object.remove("media");
            }
        } else if let Some(slot) = message.get_mut("media") {
            *slot = canonical;
        }
    }
}

/// Adds `missing` to a saved attachment whose file is gone, so a restored
/// transcript shows the state before the next request is built. A render whose
/// file the archive evicted keeps its path and is marked `source_render_missing`;
/// nothing resolves a render during a request, so the turn still builds.
fn mark_missing_media(messages: &mut [Value]) {
    for message in messages {
        let render_missing = message
            .get("source_render")
            .and_then(Value::as_str)
            .is_some_and(|path| !path.is_empty() && !Path::new(path).is_file());
        if render_missing {
            message["source_render_missing"] = json!(true);
        }
        let Some(media) = message.get_mut("media").and_then(Value::as_array_mut) else {
            continue;
        };
        for entry in media {
            let path = entry
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !path.is_empty() && !Path::new(path).is_file() {
                entry["missing"] = json!(true);
            }
        }
    }
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

/// The tool blocks of a conversation: each assistant message that asks for tools
/// plus the tool results (and any attached delivery) that follow it.
fn tool_blocks(view: &[Value]) -> Vec<(usize, usize)> {
    let mut blocks = Vec::new();
    let mut index = 0;
    while index < view.len() {
        let calls = view[index]["tool_calls"]
            .as_array()
            .map(|calls| !calls.is_empty())
            .unwrap_or(false);
        if view[index]["role"] == "assistant" && calls {
            let mut end = index + 1;
            while end < view.len()
                && (view[end]["role"] == "tool"
                    || view[end].get("input_audio").is_some()
                    || view[end].get("media").is_some())
            {
                end += 1;
            }
            blocks.push((index, end));
            index = end;
        } else {
            index += 1;
        }
    }
    blocks
}

/// The first index a cut at `keep` may keep: a cut that lands on a tool result
/// moves back to the call that opened the block. A conversation that keeps a
/// call without the results it owes is one a provider refuses, and a kept call
/// whose results were dropped would be closed as unanswered.
fn block_start(view: &[Value], keep: usize) -> usize {
    if keep >= view.len() || view[keep]["role"] != "tool" {
        return keep;
    }
    for (start, end) in tool_blocks(view) {
        if start < keep && keep < end {
            return start;
        }
    }
    keep
}

/// Closes a last round whose calls never got a result: a window that reloaded
/// mid-turn, or a client that died, stops the loop that owed them. `answered`
/// names the calls the request being prepared brings results for, which are not
/// debts. Returns the results it added, in call order; a round that follows the
/// block is complete and is left alone.
fn close_unanswered_round(view: &mut Vec<Value>, answered: &[&str]) -> Vec<Value> {
    let Some((start, end)) = tool_blocks(view).last().copied() else {
        return Vec::new();
    };
    if end < view.len() {
        return Vec::new();
    }
    let calls = view[start]["tool_calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let known: Vec<&str> = view[start + 1..end]
        .iter()
        .filter_map(|message| message["tool_call_id"].as_str())
        .chain(answered.iter().copied())
        .collect();
    let mut closed = Vec::new();
    for call in &calls {
        let Some(id) = call["id"].as_str() else {
            continue;
        };
        if known.contains(&id) {
            continue;
        }
        let name = call["function"]["name"].as_str().unwrap_or("the tool");
        closed.push(json!({
            "role": "tool",
            "tool_call_id": id,
            "content": format!(
                "Not run: the window that was running this turn went away before {name} returned, so it has no result."
            ),
        }));
    }
    for message in &mut closed {
        message_protocol::tag_new(message);
    }
    view.extend(closed.iter().cloned());
    closed
}

/// A known host-context marker. A row carrying one is never user prose.
fn is_context_marker(value: &Value) -> bool {
    matches!(value.as_str(), Some(CONTEXT_AUDITION | CONTEXT_SUMMARY))
}

fn context_of(message: &Value) -> Option<&str> {
    message.get("context").and_then(Value::as_str)
}

/// Legacy fixture helper; active readers use versioned message kinds.
#[cfg(test)]
fn is_round_nudge(content: &str) -> bool {
    message_protocol::kind(&json!({"role":"user","content":content}))
        == Some(MessageKind::RoundBudget)
}

fn is_audition(message: &Value) -> bool {
    message_protocol::kind(message).is_some_and(message_protocol::is_audition)
}

fn plain_message(message: &Value) -> Option<Value> {
    let role = message["role"].as_str()?;
    if role != "user" && role != "assistant" {
        return None;
    }
    if message.get("tool_calls").is_some()
        || message.get("provider_state").is_some()
        || message.get("media").is_some()
        || message.get("input_audio").is_some()
        || context_of(message).is_some()
    {
        return None;
    }
    let content = message["content"].as_str()?;
    if content.trim().is_empty()
        || !matches!(
            message_protocol::kind(message),
            Some(MessageKind::User | MessageKind::Assistant)
        )
    {
        return None;
    }
    let mut plain = json!({ "role": role, "content": content });
    message_protocol::tag_new(&mut plain);
    Some(plain)
}

/// Compacted messages retain prose and validated file descriptors, never tool
/// calls, provider state or signatures. Audio content remains available on disk.
pub fn strips_replay_state(messages: &[Value]) -> bool {
    messages.iter().all(|message| {
        message.as_object().is_some_and(|object| {
            object.keys().all(|key| {
                matches!(
                    key.as_str(),
                    "role" | "content" | "media" | "context" | "source_render" | "message_meta"
                )
            }) && object
                .get("message_meta")
                .is_none_or(|_| message_protocol::read(message).is_some())
                && object.get("context").is_none_or(is_context_marker)
                && object
                    .get("source_render")
                    .is_none_or(|_| audition_render_reference(message).is_some())
                && object
                    .get("media")
                    .is_none_or(|media| !canonical_media(media).as_array().unwrap().is_empty())
                && object.get("content").and_then(Value::as_str).is_some()
                && matches!(
                    object.get("role").and_then(Value::as_str),
                    Some("user") | Some("assistant")
                )
        })
    })
}

fn plan_compaction(
    view: &[Value],
    target: u64,
    facts: &ProjectFacts,
    id: &str,
) -> Result<Compaction, String> {
    if view.len() < 3 {
        return Err("This session has nothing older to compact.".into());
    }
    let before = estimate_tokens(view);
    let blocks = tool_blocks(view);
    let at = now();
    let mut keep = RETAINED_TOOL_PAIRS.min(blocks.len());
    loop {
        let retained: &[(usize, usize)] = if keep == 0 {
            &[]
        } else {
            &blocks[blocks.len() - keep..]
        };
        let split = match retained.first() {
            Some((start, _)) => *start,
            None => view.len().saturating_sub(2).max(1),
        };
        if split == 0 {
            return Err("This session has nothing older to compact.".into());
        }
        let summary = summary_text(view, split, retained, facts, before, at.as_str());
        let mut messages = Vec::with_capacity(1 + retained.len() + view.len() - split);
        let mut summary_row =
            json!({ "role": "user", "content": summary, "context": CONTEXT_SUMMARY });
        message_protocol::tag(&mut summary_row, summary_metadata(view, split, retained));
        messages.push(summary_row);
        // Preserve the latest labeled pair in a retained round. For other
        // auditions use a single row to keep replay bounded.
        for (start, end) in retained {
            if let Some(pair) = retained_comparison_pair(&view[*start..*end]) {
                messages.extend(pair.iter().map(|row| {
                    let mut kept = json!({
                        "role": "user",
                        "context": CONTEXT_AUDITION,
                        "content": clip(row["content"].as_str().unwrap_or_default(), 512),
                        "media": canonical_media(&row["media"]),
                    });
                    if let Some(meta) = message_protocol::legacy(row) {
                        message_protocol::tag(&mut kept, meta);
                    }
                    kept
                }));
            } else if let Some(row) = retained_media_row(view, *start, *end) {
                messages.push(row);
            }
        }
        messages.extend(tail_messages(view, split, retained));
        // Keep a bounded set of reference paths when their prose is folded.
        let mut paths: std::collections::HashSet<String> = messages
            .iter()
            .filter_map(|message| message["media"].as_array())
            .flatten()
            .filter_map(|entry| entry["path"].as_str().map(str::to_owned))
            .collect();
        let local_references: Vec<Value> = view
            .iter()
            .rev()
            .filter_map(|message| message["media"].as_array())
            .flat_map(|entries| entries.iter().rev())
            .filter(|entry| entry["local_reference"] == true)
            .filter(|entry| {
                entry["path"]
                    .as_str()
                    .is_some_and(|path| paths.insert(path.to_owned()))
            })
            .take(super::media::MAX_ATTACHMENTS_PER_MESSAGE)
            .cloned()
            .collect();
        if !local_references.is_empty() {
            messages[0]["media"] = json!(local_references);
        }
        if !strips_replay_state(&messages) {
            return Err("Compaction produced a message that still replays provider state.".into());
        }
        let after = estimate_tokens(&messages);
        if after < before && (target == 0 || after <= target) {
            return Ok(Compaction {
                id: id.to_owned(),
                compacted_at: at,
                messages,
                dropped_from: 0,
                dropped_to: split,
                tokens_before: before,
                tokens_after: after,
                retained_pairs: keep,
            });
        }
        if keep > 0 {
            keep -= 1;
            continue;
        }
        if after >= before {
            return Err("Compaction would not reduce this conversation.".into());
        }
        // Reduced as far as the retained pairs allow but still above the target:
        // a smaller summary is not available without losing the standing intent.
        return Ok(Compaction {
            id: id.to_owned(),
            compacted_at: at,
            messages,
            dropped_from: 0,
            dropped_to: split,
            tokens_before: before,
            tokens_after: after,
            retained_pairs: 0,
        });
    }
}

/// The messages after `split` that are safe to keep: plain text only, never a
/// message of a retained tool block, and always the pending user message.
fn tail_messages(view: &[Value], split: usize, retained: &[(usize, usize)]) -> Vec<Value> {
    let in_block = |index: usize| {
        retained
            .iter()
            .any(|(start, end)| index >= *start && index < *end)
    };
    let mut tail: Vec<Value> = (split..view.len())
        .filter(|index| !in_block(*index))
        .filter_map(|index| plain_message(&view[index]))
        .collect();
    let pending = view
        .iter()
        .rposition(|message| message["role"] == "user" && plain_message(message).is_some());
    if let Some(index) = pending {
        let message = plain_message(&view[index]).expect("checked above");
        if !retained
            .iter()
            .any(|(start, end)| index >= *start && index < *end)
            && !tail.contains(&message)
        {
            tail.push(message);
        }
    }
    tail
}

/// Every attachment a retained round references, deduplicated within the round.
fn block_media(view: &[Value], start: usize, end: usize) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    for message in &view[start..end] {
        let Some(media) = message.get("media").and_then(Value::as_array) else {
            continue;
        };
        for entry in canonical_media(&json!(media))
            .as_array()
            .into_iter()
            .flatten()
        {
            if !entries.contains(entry) {
                entries.push(entry.clone());
            }
        }
    }
    entries
}

fn retained_comparison_pair(rows: &[Value]) -> Option<&[Value]> {
    rows.windows(2).rev().find(|pair| {
        pair.iter()
            .zip([
                MessageKind::ComparisonReference,
                MessageKind::ComparisonCandidate,
            ])
            .all(|(row, expected)| {
                message_protocol::kind(row) == Some(expected)
                    && row["media"]
                        .as_array()
                        .is_some_and(|media| !media.is_empty())
            })
    })
}

/// One audio row for a retained round: the round's clips, the label it
/// carried, and the marker that keeps the row out of user prose.
fn retained_media_row(view: &[Value], start: usize, end: usize) -> Option<Value> {
    let entries = block_media(view, start, end);
    if entries.is_empty() {
        return None;
    }
    let latest = view[start..end]
        .iter()
        .rev()
        .find(|message| audition_render_reference(message).is_some())
        .or_else(|| {
            view[start..end]
                .iter()
                .find(|message| message.get("media").is_some())
        });
    let label = latest
        .and_then(|message| message["content"].as_str())
        .unwrap_or("Saved audio attachment");
    let mut row = json!({
        "role": "user",
        "content": clip(label, 512),
        "context": CONTEXT_AUDITION,
        "media": entries,
    });
    let mut meta = latest
        .and_then(message_protocol::legacy)
        .unwrap_or_else(|| MessageMeta::new(MessageKind::Audition));
    if !message_protocol::is_audition(meta.kind) {
        meta = MessageMeta::new(MessageKind::Audition);
    }
    message_protocol::tag(&mut row, meta);
    if let Some(path) = latest.and_then(audition_render_reference) {
        row["source_render"] = json!(path);
    }
    Some(row)
}

/// The attachments compaction carries: one round's entries, newest last.
fn retained_media(view: &[Value], retained: &[(usize, usize)]) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    for (start, end) in retained {
        let block = &view[*start..*end];
        let kept = retained_comparison_pair(block).unwrap_or(block);
        for entry in block_media(kept, 0, kept.len()) {
            if !entries.contains(&entry) {
                entries.push(entry);
            }
        }
    }
    entries
}

/// Auditions this compaction discards: rows outside the retained rounds, plus
/// the count a previous summary already folded so repeated compaction keeps it.
fn dropped_audition_count(view: &[Value], retained: &[(usize, usize)]) -> usize {
    let in_retained = |index: usize| {
        retained
            .iter()
            .any(|(start, end)| index >= *start && index < *end)
    };
    let rows = view
        .iter()
        .enumerate()
        .filter(|(index, message)| !in_retained(*index) && is_audition(message))
        .count();
    let previous = view
        .iter()
        .filter_map(message_protocol::legacy)
        .filter(|meta| meta.kind == MessageKind::Summary)
        .find_map(|meta| meta.discarded_auditions)
        .unwrap_or(0);
    rows + previous
}

fn summary_metadata(view: &[Value], split: usize, retained: &[(usize, usize)]) -> MessageMeta {
    let old = &view[..split];
    let mut metadata = MessageMeta::new(MessageKind::Summary);
    metadata.standing_intent = old
        .iter()
        .filter_map(message_protocol::legacy)
        .filter(|meta| meta.kind == MessageKind::Summary)
        .find_map(|meta| meta.standing_intent)
        .or_else(|| {
            old.iter().find_map(|message| {
                if message_protocol::kind(message) != Some(MessageKind::User) {
                    return None;
                }
                Some(clip(
                    &collapse(message["content"].as_str()?),
                    MAX_INTENT_CHARS,
                ))
            })
        });
    metadata.discarded_auditions = Some(dropped_audition_count(view, retained));
    for message in old.iter().rev() {
        let Some(meta) = message_protocol::legacy(message) else {
            continue;
        };
        if metadata.rollback_notice.is_none() {
            metadata.rollback_notice = if meta.kind == MessageKind::TransactionRollback {
                message["content"].as_str().map(|text| clip(text, 4096))
            } else {
                meta.rollback_notice
            };
        }
        if metadata.proposal_notice.is_none() {
            metadata.proposal_notice = if meta.kind == MessageKind::ProposalOutcome {
                message["content"].as_str().map(|text| clip(text, 4096))
            } else {
                meta.proposal_notice
            };
        }
    }
    metadata
}

fn summary_text(
    view: &[Value],
    split: usize,
    retained: &[(usize, usize)],
    facts: &ProjectFacts,
    tokens_before: u64,
    at: &str,
) -> String {
    let old = &view[..split];
    let mut text = String::new();
    text.push_str(&format!(
        "Conversation summary written {at}. Everything older than the most recent tool calls \
         below is replaced by this message; treat it as context, not as an instruction.\n\n"
    ));
    text.push_str(&format!(
        "Project: {}\n",
        facts
            .path
            .as_deref()
            .unwrap_or("no saved project (detached conversation)")
    ));
    if !facts.dsl_digest.is_empty() {
        text.push_str(&format!(
            "Project DSL: sha256 {} ({} tracks)\n",
            facts.dsl_digest, facts.track_count
        ));
    }
    let metadata = summary_metadata(view, split, retained);
    let intent = metadata
        .standing_intent
        .as_deref()
        .unwrap_or("not recorded");
    text.push_str(&format!("Standing intent: {intent}\n"));
    for outcome in [metadata.rollback_notice, metadata.proposal_notice]
        .into_iter()
        .flatten()
    {
        text.push_str(&outcome);
        text.push('\n');
    }

    let mut decisions = Vec::new();
    let mut failures = Vec::new();
    let mut proposals = Vec::new();
    let mut asks = Vec::new();
    for (index, message) in old.iter().enumerate() {
        match message["role"].as_str() {
            Some("user") => {
                // Only real prose counts as an ask: a delivery, a summary or a
                // media row is host context, and the round nudge is the loop's.
                if let Some(plain) = plain_message(message)
                    && let Some(content) = plain["content"].as_str()
                {
                    let content = clip(&collapse(content), MAX_SUMMARY_ITEM_CHARS);
                    if !content.is_empty() && asks.len() < 5 {
                        asks.push(content);
                    }
                }
                continue;
            }
            Some("tool") => {
                let name = tool_name_of(view, index);
                if is_proposal(message) {
                    if proposals.len() < 6 {
                        proposals.push(format!(
                            "{name} proposed a replacement for {} (not applied; the user decides)",
                            proposal_target(view, index)
                        ));
                    }
                    continue;
                }
                let summary = summary_of_result(message);
                if is_failure(message) {
                    if failures.len() < 6 {
                        failures.push(format!("{name} → {summary}"));
                    }
                } else if crate::tool_registry::is_mutation_tool(&name) && decisions.len() < 6 {
                    decisions.push(format!("{name} reported success at that time ({summary}); consult current project state"));
                }
                continue;
            }
            _ => {}
        }
    }
    text.push_str("Decisions taken:\n");
    if decisions.is_empty() {
        text.push_str("- none recorded\n");
    }
    for decision in &decisions {
        text.push_str(&format!("- {decision}\n"));
    }
    if !proposals.is_empty() {
        text.push_str("Proposals awaiting the user:\n");
        for proposal in &proposals {
            text.push_str(&format!("- {proposal}\n"));
        }
    }
    text.push_str("What was tried and failed:\n");
    if failures.is_empty() {
        text.push_str("- nothing failed\n");
    }
    for failure in &failures {
        text.push_str(&format!("- {failure}\n"));
    }
    let open = old
        .iter()
        .rev()
        .find(|message| message["role"] == "assistant")
        .and_then(|message| message["content"].as_str())
        .map(|content| clip(&collapse(content), MAX_SUMMARY_ITEM_CHARS * 2))
        .unwrap_or_else(|| "none recorded".into());
    text.push_str(&format!("Open questions:\n- {open}\n"));
    if !asks.is_empty() {
        text.push_str("Earlier user asks:\n");
        for ask in &asks {
            text.push_str(&format!("- \"{ask}\"\n"));
        }
    }
    text.push_str(&format!(
        "Retained recent tool calls (newest {}, oldest first):\n",
        retained.len()
    ));
    if retained.is_empty() {
        text.push_str("- none retained\n");
    }
    for (offset, (start, end)) in retained.iter().enumerate() {
        let name = view[*start]["tool_calls"]
            .as_array()
            .and_then(|calls| calls.first())
            .and_then(|call| call["function"]["name"].as_str())
            .unwrap_or("tool")
            .to_owned();
        let arguments = view[*start]["tool_calls"]
            .as_array()
            .and_then(|calls| calls.first())
            .and_then(|call| call["function"]["arguments"].as_str())
            .map(|arguments| clip(&collapse(arguments), MAX_SUMMARY_ITEM_CHARS))
            .unwrap_or_default();
        let result = view
            .get(*start + 1)
            .filter(|message| message["role"] == "tool")
            .map(summary_of_result)
            .unwrap_or_else(|| "no result recorded".into());
        let _ = end;
        text.push_str(&format!(
            "{}. {name} → {arguments} → {result}\n",
            offset + 1
        ));
    }
    let kept = retained_media(view, retained);
    if !kept.is_empty() {
        let newest = kept
            .last()
            .and_then(|entry| entry["name"].as_str())
            .unwrap_or("audio clip");
        text.push_str(&format!(
            "Audio kept on disk: {} clip(s), newest {newest}.\n",
            kept.len()
        ));
    }
    let dropped = dropped_audition_count(view, retained);
    if dropped > 0 {
        text.push_str(&format!(
            "Discarded audition deliveries: {dropped} clips (their files remain on disk; ask for a range again to listen).\n"
        ));
    }
    text.push_str(&format!(
        "Dropped messages 0..{split} of {total} (~{tokens_before} tokens before compaction); \
         this summary replaces them.\n",
        total = view.len()
    ));
    artifacts::append(&mut text, view);
    text
}

fn tool_name_of(view: &[Value], index: usize) -> String {
    let call_id = view[index]["tool_call_id"].as_str().unwrap_or_default();
    for message in view[..index].iter().rev() {
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                if call["id"].as_str() == Some(call_id) {
                    return call["function"]["name"]
                        .as_str()
                        .unwrap_or("tool")
                        .to_owned();
                }
            }
        }
    }
    "tool".into()
}

/// A tool result that proposed something and was not applied. A rejected
/// proposal is history like any other turn: the summary says so in text and no
/// tool-call turn is replayed.
fn is_proposal(message: &Value) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(message["content"].as_str().unwrap_or_default())
    else {
        return false;
    };
    value.get("status").and_then(Value::as_str) == Some("pending_user_confirmation")
        || value.get("applied") == Some(&Value::Bool(false))
}

/// The subtree a proposal named, from the arguments of the call that made it.
fn proposal_target(view: &[Value], index: usize) -> String {
    let call_id = view[index]["tool_call_id"].as_str().unwrap_or_default();
    for message in view[..index].iter().rev() {
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                if call["id"].as_str() != Some(call_id) {
                    continue;
                }
                let arguments = serde_json::from_str::<Value>(
                    call["function"]["arguments"].as_str().unwrap_or_default(),
                )
                .ok();
                if let Some(path) = arguments
                    .as_ref()
                    .and_then(|arguments| arguments.get("path"))
                    .and_then(Value::as_str)
                {
                    return path.to_owned();
                }
                return call["function"]["name"]
                    .as_str()
                    .unwrap_or("a subtree")
                    .to_owned();
            }
        }
    }
    "a subtree".into()
}

fn is_failure(message: &Value) -> bool {
    let content = message["content"].as_str().unwrap_or_default();
    if content.trim_start().starts_with('{')
        && let Ok(value) = serde_json::from_str::<Value>(content)
    {
        return value.get("error").is_some() || value.get("ok") == Some(&Value::Bool(false));
    }
    content.contains("\"error\"") || content.starts_with("failed")
}

fn summary_of_result(message: &Value) -> String {
    let content = message["content"].as_str().unwrap_or_default();
    if let Ok(value) = serde_json::from_str::<Value>(content) {
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return clip(
                &format!("failed: {}", collapse(error)),
                MAX_SUMMARY_RESULT_CHARS,
            );
        }
        if is_proposal(message) {
            return "proposed, not applied".into();
        }
        if let Some(object) = value.as_object() {
            let values: serde_json::Map<String, Value> = object
                .iter()
                .filter(|(_, value)| {
                    value.is_number()
                        || value.is_boolean()
                        || value.as_str().is_some_and(|text| text.len() <= 80)
                })
                .take(6)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            if values.is_empty() {
                return "Structured result; see saved artifact inventory for paths and findings"
                    .into();
            }
            return clip(
                &format!("ok: {}", Value::Object(values)),
                MAX_SUMMARY_RESULT_CHARS,
            );
        }
        if value.is_array() {
            return format!("ok: {} items", value.as_array().map(Vec::len).unwrap_or(0));
        }
    }
    if content.trim().is_empty() {
        return "ok: empty".into();
    }
    clip(&collapse(content), MAX_SUMMARY_RESULT_CHARS)
}

fn should_compact(view: &[Value], overhead: u64, context_length: u64) -> bool {
    if view.len() > MAX_AUTO_COMPACT_MESSAGES {
        return true;
    }
    if context_length == 0 {
        return false;
    }
    let tokens = estimate_tokens(view);
    let limit = (context_length as f64 * AUTO_COMPACT_FRACTION) as u64;
    let floor = (context_length as f64 * AUTO_COMPACT_CONVERSATION_FLOOR) as u64;
    tokens + overhead > limit.max(1) && tokens > floor
}

// ---- token estimate -----------------------------------------------------

/// An estimate, not a tokenizer: about four ASCII characters per token, one
/// token per non-ASCII character (CJK text tokenizes near one per character),
/// plus a small per-message overhead. Monotone in the messages it is given.
pub fn estimate_tokens(messages: &[Value]) -> u64 {
    messages.iter().map(message_tokens).sum()
}

fn message_tokens(message: &Value) -> u64 {
    let mut tokens = 4;
    if let Some(content) = message["content"].as_str() {
        tokens += text_tokens(content);
    }
    if let Some(calls) = message["tool_calls"].as_array() {
        for call in calls {
            tokens += text_tokens(call["function"]["name"].as_str().unwrap_or_default());
            tokens += text_tokens(call["function"]["arguments"].as_str().unwrap_or_default());
        }
    }
    // Reasoning is replayed to the provider, so it occupies the context too.
    if let Some(thinking) = message["thinking"].as_array() {
        for entry in thinking {
            tokens += text_tokens(entry["text"].as_str().unwrap_or_default());
            tokens += text_tokens(entry["signature"].as_str().unwrap_or_default());
        }
    }
    tokens
}

fn text_tokens(text: &str) -> u64 {
    let mut units = 0u64;
    for character in text.chars() {
        units += if character.is_ascii() { 1 } else { 4 };
    }
    units.div_ceil(4)
}

// ---- file I/O -----------------------------------------------------------

fn read_meta(path: &Path) -> Option<SessionMeta> {
    lookup::read_meta(path).ok().flatten()
}

fn write_meta(dir: &Path, meta: &SessionMeta) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(meta).map_err(|error| error.to_string())?;
    private_fs::write_atomic(&dir.join(META_FILE), &bytes)
        .map_err(|error| format!("sessions: {error}"))
}

/// The conversation the records add up to, plus how many lines were unreadable.
/// Every turn the file ever recorded, with the compaction records ignored.
/// `read_view` is the conversation the model gets; this is the history the user
/// wrote, so a compacted session can still be inspected in full.
pub fn read_original(dir: &Path) -> Result<(Vec<Value>, u64, u64), String> {
    let path = dir.join(MESSAGES_FILE);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), 0, 0));
        }
        Err(error) => return Err(format!("sessions: {error}")),
    };
    let mut messages = Vec::new();
    let mut compactions = 0;
    let mut skipped = 0;
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|error| format!("sessions: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&line) {
            Ok(record) => match record.kind {
                RecordKind::Turn => messages.extend(record.messages),
                RecordKind::Compaction => compactions += 1,
                RecordKind::Trim => {}
            },
            Err(_) => skipped += 1,
        }
    }
    canonicalize_media(&mut messages);
    message_protocol::migrate(&mut messages);
    artifacts::repair(&mut messages);
    Ok((messages, compactions, skipped))
}

/// The metadata of one session directory, for callers that only need its name
/// and size. `None` when the directory is not a session this build can read.
pub fn read_meta_for_storage(dir: &Path) -> Option<SessionMeta> {
    read_meta(&dir.join(META_FILE))
}

/// Whether the file holds a compaction record, without building the view.
fn has_compaction(dir: &Path) -> bool {
    let path = dir.join(MESSAGES_FILE);
    let Ok(file) = std::fs::File::open(&path) else {
        return false;
    };
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else { return false };
        if line.contains("\"kind\":\"compaction\"") {
            return true;
        }
    }
    false
}

fn read_view(dir: &Path) -> Result<(Vec<Value>, u64), String> {
    let path = dir.join(MESSAGES_FILE);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(error) => return Err(format!("sessions: {error}")),
    };
    let mut view = Vec::new();
    let mut skipped = 0;
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|error| format!("sessions: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&line) {
            Ok(record) => match record.kind {
                RecordKind::Turn => view.extend(record.messages),
                RecordKind::Compaction | RecordKind::Trim => view = record.messages,
            },
            Err(_) => skipped += 1,
        }
    }
    canonicalize_media(&mut view);
    message_protocol::migrate(&mut view);
    artifacts::repair(&mut view);
    Ok((view, skipped))
}

/// Every record in order. Unlike [`read_view`] this keeps the compaction and
/// trim records, so a rewrite can decide what to keep.
fn read_records(dir: &Path) -> Result<Vec<Record>, String> {
    let path = dir.join(MESSAGES_FILE);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("sessions: {error}")),
    };
    let mut records = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|error| format!("sessions: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<Record>(&line) {
            records.push(record);
        }
    }
    Ok(records)
}

fn write_records(dir: &Path, records: &[Record]) -> Result<(), String> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(
            serde_json::to_vec(record)
                .map_err(|error| format!("sessions: {error}"))?
                .as_slice(),
        );
        bytes.push(b'\n');
    }
    private_fs::write_atomic(&dir.join(MESSAGES_FILE), &bytes)
        .map_err(|error| format!("sessions: {error}"))
}

/// Appends records as lines, keeping the file under [`MAX_SESSION_BYTES`]. An
/// append that would exceed the cap first replaces the conversation with a
/// `trim` record that states what it dropped.
fn append_records(dir: &Path, records: &[Record]) -> Result<(), String> {
    append_records_under(dir, records, MAX_SESSION_BYTES)
}

/// The cap is a parameter so the trimming path is testable without writing a
/// gigabyte.
fn append_records_under(dir: &Path, records: &[Record], cap: u64) -> Result<(), String> {
    let path = dir.join(MESSAGES_FILE);
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(
            serde_json::to_vec(record)
                .map_err(|error| format!("sessions: {error}"))?
                .as_slice(),
        );
        bytes.push(b'\n');
    }
    let current = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    if current + bytes.len() as u64 <= cap {
        return private_fs::append_private(&path, &bytes)
            .map_err(|error| format!("sessions: {error}"));
    }
    let (mut view, _) = read_view(dir)?;
    for record in records {
        match record.kind {
            RecordKind::Turn => view.extend(record.messages.clone()),
            RecordKind::Compaction | RecordKind::Trim => view = record.messages.clone(),
        }
    }
    let kept = newest_that_fit(&view, cap / 2);
    if kept.is_empty() {
        return Err("This turn is larger than a session may hold.".into());
    }
    let mut note = json!({
        "role": "user",
        "content": format!(
            "Earlier messages of this session were dropped to keep it under {} bytes. \
             Messages {}..{} are gone; the conversation resumes below.",
            cap,
            view.len() - kept.len(),
            view.len()
        )
    });
    message_protocol::tag(&mut note, MessageMeta::new(MessageKind::HostNotice));
    let mut messages = vec![note];
    messages.extend(kept);
    let trim = Record {
        v: VERSION,
        kind: RecordKind::Trim,
        at: now(),
        dropped: Some(Dropped {
            from: 0,
            to: view.len() - messages.len() + 1,
        }),
        tokens_before: Some(estimate_tokens(&view)),
        tokens_after: Some(estimate_tokens(&messages)),
        truncated: Some(true),
        messages,
    };
    write_records(dir, &[trim])
}

/// The newest messages of a conversation that fit in `budget` bytes, in order.
fn newest_that_fit(view: &[Value], budget: u64) -> Vec<Value> {
    let mut kept: Vec<Value> = Vec::new();
    let mut used = 0u64;
    for message in view.iter().rev() {
        let size = serde_json::to_vec(message)
            .map(|bytes| bytes.len())
            .unwrap_or(0) as u64
            + 1;
        if used + size > budget && !kept.is_empty() {
            break;
        }
        used += size;
        kept.push(message.clone());
    }
    kept.reverse();
    kept
}

// ---- helpers ------------------------------------------------------------

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

/// A directory as the store keys it; a path that does not exist yet stays as
/// given so a project saved later keeps its key.
fn canonical_dir(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn canonical_path(path: &Path) -> PathBuf {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return resolved;
    }
    // A project that is not saved yet, or one that moved: resolve the directory
    // so the key of a missing file still matches the key it had when it existed.
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

fn hashed_key(path: &Path) -> String {
    let canonical = canonical_path(path);
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    let mut key = String::with_capacity(16);
    for byte in &digest[..8] {
        key.push_str(&format!("{byte:02x}"));
    }
    key
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

fn is_valid_key(key: &str) -> bool {
    key == DETACHED_KEY || (key.len() == 16 && is_valid_id(key))
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

// ---- test support -------------------------------------------------------

/// The panel's process-wide active session per project key, so a turn can
/// autosave without the panel naming a session.
pub fn active(key: &str) -> Option<String> {
    ACTIVE
        .lock()
        .ok()
        .and_then(|active| active.get(key).cloned())
}

pub fn set_active(key: &str, id: &str) {
    if let Ok(mut active) = ACTIVE.lock() {
        active.insert(key.to_owned(), id.to_owned());
    }
}

#[cfg(test)]
pub(crate) struct TestRootGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
pub(crate) fn test_root(root: PathBuf) -> TestRootGuard {
    let lock = TEST_ROOT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(mut slot) = TEST_ROOT.lock() {
        *slot = Some(root);
    }
    TestRootGuard { _lock: lock }
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
        append_records(&dir, &[turn, compaction]).unwrap();

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
mod tests {
    use super::*;
    use crate::ai::auth::Credential;
    use crate::ai::test_support::canned_by_request;
    use crate::ai::{self, provider_for, request_from_wire};
    use crate::commands::{AiCapabilities, AiProviderEntry, AuthKind, ProviderKind, Wire};

    fn user(text: &str) -> Value {
        json!({ "role": "user", "content": text })
    }

    fn reply(text: &str) -> Value {
        json!({ "role": "assistant", "content": text })
    }

    fn expected_user(text: &str) -> Value {
        json!({"role":"user", "content":text, "message_meta":{"version":1,"kind":"user"}})
    }

    fn expected_reply(text: &str) -> Value {
        json!({"role":"assistant", "content":text, "message_meta":{"version":1,"kind":"assistant"}})
    }

    /// An assistant turn as a provider that requires thought signatures sends it.
    fn tool_call(name: &str, arguments: &str, signature: &str) -> Value {
        json!({
            "role": "assistant",
            "content": null,
            "provider_state": [{"type": "reasoning", "encrypted_content": "opaque-continuity"}],
            "tool_calls": [{
                "id": format!("call-{name}"),
                "type": "function",
                "function": { "name": name, "arguments": arguments },
                "extra_content": {"google": {"thought_signature": signature}},
                "signature": signature
            }]
        })
    }

    fn tool_result(name: &str, content: &str) -> Value {
        json!({ "role": "tool", "tool_call_id": format!("call-{name}"), "content": content })
    }

    /// The workspace a fixture's project file lives in.
    fn workspace(project: &Path) -> PathBuf {
        project.parent().unwrap().to_path_buf()
    }

    fn facts(project: &Path) -> ProjectFacts {
        ProjectFacts {
            path: Some(project.display().to_string()),
            dsl_digest: dsl_digest("@project\n  tempo: 128\n"),
            track_count: 18,
        }
    }

    fn conversation() -> Vec<Value> {
        let mut view = vec![user("make the kick punchier and add a pad")];
        for step in 0..8 {
            let name = if step % 2 == 0 {
                "tracks_mix"
            } else {
                "render_range"
            };
            let arguments = format!("{{\"track\":\"kick\",\"gain_db\":{}}}", -step);
            view.push(tool_call(name, &arguments, &format!("sig-{step}")));
            let result = if step == 3 {
                json!({ "error": "track 'pad' not found" }).to_string()
            } else {
                // Tool payloads are what a real turn spends its context on; the
                // summary has to be smaller than they are for compaction to pay.
                let notes: Vec<String> = (0..12)
                    .map(|index| format!("floor(step={step}, band={index}): -{index}.5 dB"))
                    .collect();
                json!({ "ok": true, "tracks": 18, "step": step, "notes": notes }).to_string()
            };
            view.push(tool_result(name, &result));
            view.push(user(&format!("and now step {step}")));
            view.push(reply(&format!("step {step} done")));
        }
        view.push(user("now the bass"));
        view
    }

    #[test]
    fn proposal_checkpoint_preserves_pending_deferred_and_applied_on_continue() {
        let (store, project) = fixture("proposal-checkpoint");
        let meta = store
            .create(None, Some(&project), Some("Goal"), None)
            .unwrap();
        let mut call = tool_call("edit_subtree", "{}", "sig");
        let second = json!({"id":"later","type":"function","function":{"name":"edit_subtree","arguments":"{}"}});
        call["tool_calls"].as_array_mut().unwrap().push(second);
        let history = vec![user("Improve the song"), call];
        let current = facts(&project);
        store
            .prepare(
                &meta.id,
                &history,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 1_000_000,
                    facts: &current,
                    provider: "fixture",
                    model: "model",
                },
            )
            .unwrap();
        let results = vec![
            tool_result(
                "edit_subtree",
                "{\"status\":\"pending_user_confirmation\",\"applied\":false}",
            ),
            json!({"role":"tool","tool_call_id":"later","content":"{\"deferred\":true,\"applied\":false}"}),
            tool_result("unknown", "ignore"),
        ];
        store.checkpoint_tools(&meta.id, &results, None).unwrap();
        store.checkpoint_tools(&meta.id, &results, None).unwrap();
        store
            .checkpoint_tools(&meta.id, &[], Some(("applied", "track:lead_chop")))
            .unwrap();
        let prepared = store
            .prepare(
                &meta.id,
                &[user("Continue")],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 1_000_000,
                    facts: &current,
                    provider: "fixture",
                    model: "model",
                },
            )
            .unwrap();
        let text = serde_json::to_string(&prepared.messages).unwrap();
        assert!(!text.contains("Not run:"));
        assert!(text.contains("pending_user_confirmation"));
        assert!(text.contains("deferred"));
        assert!(text.contains("Host proposal outcome:"));
        assert_eq!(
            prepared
                .messages
                .iter()
                .filter(|m| m["role"] == "tool")
                .count(),
            2
        );
        let summary = summary_text(
            &prepared.messages,
            prepared.messages.len(),
            &[],
            &current,
            1000,
            "now",
        );
        assert!(summary.contains("Host proposal outcome:"));
        let reopened = Store::new(store.root().to_owned()).load(&meta.id).unwrap();
        assert_eq!(reopened.messages, prepared.messages);
        store
            .checkpoint_tools(&meta.id, &[], Some(("rejected", "track:bass")))
            .unwrap();
        assert!(
            store.load(&meta.id).unwrap().messages.last().unwrap()["content"]
                .as_str()
                .unwrap()
                .contains("rejected")
        );
    }

    #[test]
    fn rollback_survives_reopen_continue_and_repeated_compaction() {
        let (store, project) = fixture("rollback-continuity");
        let meta = store
            .create(None, Some(&project), Some("Goal"), None)
            .unwrap();
        let history = vec![
            user("Build an editable song"),
            tool_call("tracks_create", "{}", "sig"),
            tool_result("tracks_create", "{\"track_count\":16}"),
            tool_call("tracks_mix", "{}", "sig2"),
        ];
        let current = ProjectFacts {
            track_count: 1,
            ..facts(&project)
        };
        store
            .prepare(
                &meta.id,
                &history,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 1_000_000,
                    facts: &current,
                    provider: "fixture",
                    model: "model",
                },
            )
            .unwrap();
        store.record_rollback(&meta.id, &current).unwrap();
        let reopened = Store::new(store.root().to_owned());
        let loaded = reopened.load(&meta.id).unwrap();
        let notice = loaded.messages.last().unwrap()["content"].as_str().unwrap();
        assert!(notice.starts_with("Host transaction rollback:"));
        assert!(notice.contains("track count 1"));
        let mut stale = history;
        stale.push(user("Continue"));
        let prepared = reopened
            .prepare(
                &meta.id,
                &stale,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 1_000_000,
                    facts: &current,
                    provider: "fixture",
                    model: "model",
                },
            )
            .unwrap();
        assert_eq!(prepared.appended, 1);
        let first = summary_text(
            &prepared.messages,
            prepared.messages.len(),
            &[],
            &current,
            2000,
            "first",
        );
        assert!(first.contains(notice));
        assert!(!first.contains("tracks_create applied"));
        let second_view = vec![user(&first), user("Continue")];
        let second = summary_text(&second_view, 2, &[], &current, 1000, "second");
        assert!(second.contains(notice));
        assert!(second.contains("Standing intent: Build an editable song"));
    }

    #[test]
    fn repeated_compaction_preserves_standing_goal_and_artifact_values() {
        let (_, project) = fixture("artifact-intent");
        let goal = "Recreate the song with editable instruments and retain the separated vocal.";
        let view = vec![
            json!({"role":"user","content":goal}),
            json!({"role":"assistant","tool_calls":[{"id":"s","function":{"name":"separate_stems","arguments":"{\"path\":\"/source.wav\"}"}}]}),
            json!({"role":"tool","tool_call_id":"s","content":json!({"stems":[{"name":"vocals","path":"/complete/unique/vocals.wav"}]}).to_string()}),
        ];
        let first = summary_text(&view, view.len(), &[], &facts(&project), 1000, "fixture");
        let next = vec![
            json!({"role":"user","content":first}),
            json!({"role":"user","content":"Continue"}),
        ];
        let second = summary_text(&next, next.len(), &[], &facts(&project), 500, "again");
        assert!(second.contains(&format!("Standing intent: {goal}")));
        assert!(second.contains("/complete/unique/vocals.wav"));
        assert!(second.contains("/source.wav"));
    }

    #[test]
    fn bounded_audio_replay_preserves_archive_and_prioritizes_explicit_attachment() {
        let clip = |name: &str| json!({"role":"user", "content":name, "media":[{"kind":"audio","name":name,"path":format!("/archive/{name}.wav"),"sha256":name,"bytes":9_000_000}]});
        let archive = vec![clip("old"), clip("recent"), clip("explicit")];
        let mut outbound = archive.clone();
        bound_media_replay(&mut outbound, &[clip("explicit")]);
        assert_eq!(
            archive
                .iter()
                .filter(|m| m["media"].as_array().unwrap().len() == 1)
                .count(),
            3
        );
        assert_eq!(outbound[2]["media"].as_array().unwrap().len(), 1);
        assert!(outbound[0]["media"].as_array().unwrap().is_empty());
        assert!(outbound[1]["media"].as_array().unwrap().is_empty());
        assert!(
            outbound[2]["content"]
                .as_str()
                .unwrap()
                .contains("2 older attachment(s)")
        );
        assert!(
            outbound[2]["content"]
                .as_str()
                .unwrap()
                .contains("/archive/old.wav")
        );
    }

    #[test]
    fn two_sessions_for_one_workspace_and_one_detached_round_trip() {
        // `OCP_SESSIONS_KEEP=1` keeps the fixture tree and prints its root, so a
        // run can be inspected outside the test.
        let keep = std::env::var_os("OCP_SESSIONS_KEEP").is_some();
        let (store, project) = fixture("roundtrip");
        let place = workspace(&project);
        let first = store
            .create(Some(&place), Some(&project), Some("Intro idea"), None)
            .unwrap();
        let second = store
            .create(Some(&place), Some(&project), Some("Second idea"), None)
            .unwrap();
        let detached = store.create(None, None, Some("No project"), None).unwrap();

        assert_eq!(store.list(Scope::Workspace(Some(&place))).unwrap().len(), 2);
        assert_eq!(store.list(Scope::All).unwrap().len(), 3);
        assert_eq!(detached.workspace_key, DETACHED_KEY);

        let key = store.workspace_key(Some(&place));
        assert_eq!(key.len(), 16);
        assert!(store.root().join(&key).join(WORKSPACE_FILE).is_file());
        let record: Value = serde_json::from_slice(
            &std::fs::read(store.root().join(&key).join(WORKSPACE_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            record["workspace"],
            canonical_dir(&place).to_string_lossy().as_ref()
        );
        assert!(
            !store
                .root()
                .join(DETACHED_KEY)
                .join(WORKSPACE_FILE)
                .exists(),
            "a detached conversation has no workspace to record"
        );

        let first_dir = store.root().join(&key).join(&first.id);
        let context = facts(&project);
        for (session, text) in [(&first, "first"), (&second, "second")] {
            store
                .prepare(
                    &session.id,
                    &[user(text)],
                    Some(&project),
                    crate::ai::sessions::PrepareContext {
                        snapshot: None,
                        overhead: 0,
                        context_length: 0,
                        facts: &context,
                        provider: "fixture-provider",
                        model: "fixture-model",
                    },
                )
                .unwrap();
            store
                .record_reply(
                    &session.id,
                    &reply(&format!("{text} answer")),
                    &context,
                    "fixture-provider",
                    "fixture-model",
                )
                .unwrap();
        }
        let loaded = store.load(&first.id).unwrap();
        assert_eq!(
            loaded.messages,
            vec![expected_user("first"), expected_reply("first answer")]
        );
        assert_eq!(loaded.meta.turns, 1);
        assert!(loaded.meta.tokens > 0);
        assert_eq!(loaded.meta.title, "Intro idea");
        assert_eq!(store.load(&second.id).unwrap().messages.len(), 2);
        assert!(store.load(&detached.id).unwrap().messages.is_empty());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| path.metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&first_dir), 0o700);
            assert_eq!(mode(&first_dir.join(META_FILE)), 0o600);
            assert_eq!(mode(&first_dir.join(MESSAGES_FILE)), 0o600);
            assert_eq!(mode(store.root()), 0o700);
        }
        if keep {
            println!("OCP_SESSIONS_ROOT {}", store.root().display());
            return;
        }
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn sessions_list_is_newest_first_and_prunes_the_oldest() {
        let (store, project) = fixture("prune");
        let mut created = Vec::new();
        for index in 0..=MAX_SESSIONS_PER_WORKSPACE {
            created.push(
                store
                    .create(
                        Some(&workspace(&project)),
                        Some(&project),
                        Some(&format!("session {index}")),
                        None,
                    )
                    .unwrap(),
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let listed = store
            .list(Scope::Workspace(Some(&workspace(&project))))
            .unwrap();
        assert_eq!(listed.len(), MAX_SESSIONS_PER_WORKSPACE);
        let ids: Vec<&str> = listed.iter().map(|meta| meta.id.as_str()).collect();
        assert!(
            !ids.contains(&created[0].id.as_str()),
            "the least recently updated session is the one dropped"
        );
        assert_eq!(ids[0], created[created.len() - 1].id);
        let mut sorted = listed.clone();
        sorted.sort_by(|left, right| {
            right
                .updated
                .cmp(&left.updated)
                .then_with(|| right.id.cmp(&left.id))
        });
        assert_eq!(sorted, listed);
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn tool_result_pairing_survives_null_assistant_content_in_a_batch() {
        let (store, project) = fixture("multi-tool-result-pairing");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let context = facts(&project);
        let question = user("Find a piano and inspect the project");
        store
            .prepare(
                &session.id,
                std::slice::from_ref(&question),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture",
                    model: "model",
                },
            )
            .unwrap();
        let call = json!({"role":"assistant","content":null,"tool_calls":[
            {"id":"font-call","type":"function","function":{"name":"soundfonts_list","arguments":"{}"}},
            {"id":"project-call","type":"function","function":{"name":"project_get","arguments":"{}"}}
        ]});
        store
            .record_reply(&session.id, &call, &context, "fixture", "model")
            .unwrap();
        let fonts = json!({"role":"tool","tool_call_id":"font-call","content":"{\"discovered\":[\"piano.sf3\"]}","message_meta":{"version":1,"kind":"tool"}});
        let project_result = json!({"role":"tool","tool_call_id":"project-call","content":"{\"project\":\"Evening Sketch\"}","message_meta":{"version":1,"kind":"tool"}});
        let prepared = store
            .prepare(
                &session.id,
                &[question, call, fonts.clone(), project_result.clone()],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture",
                    model: "model",
                },
            )
            .unwrap();
        assert_eq!(prepared.appended, 2);
        assert_eq!(&prepared.messages[2..], &[fonts, project_result]);
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn tool_result_pairing_does_not_match_different_empty_assistant_calls() {
        let first = json!({"role":"assistant","content":null,"tool_calls":[{"id":"a","function":{"name":"project_get","arguments":"{}"}}]});
        let second = json!({"role":"assistant","content":"","tool_calls":[{"id":"b","function":{"name":"soundfonts_list","arguments":"{}"}}]});
        assert!(!same_turn(&first, &second));
        assert!(same_turn(&first, &saved_message(&first).unwrap()));
        assert!(!same_turn(
            &json!({"role":"tool","content":"{}","tool_call_id":"a"}),
            &json!({"role":"tool","content":"{}","tool_call_id":"b"})
        ));
    }

    #[test]
    fn autosave_appends_a_turn_once_and_never_replays_the_rebuilt_history() {
        let (store, project) = fixture("autosave");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let context = facts(&project);
        let system = json!({ "role": "system", "content": "you are the panel" });

        let turn = json!([system, user("make it punchy")]);
        let prepared = store
            .prepare(
                &session.id,
                turn.as_array().unwrap(),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        assert_eq!(prepared.appended, 1);
        assert!(prepared.live.is_none());
        assert_eq!(prepared.messages, vec![expected_user("make it punchy")]);

        let call = tool_call("tracks_mix", "{\"track\":\"kick\"}", "sig-0");
        store
            .record_reply(
                &session.id,
                &call,
                &context,
                "fixture-provider",
                "fixture-model",
            )
            .unwrap();
        // Round 2 of the same turn replays the assistant call and adds one result.
        let round = json!([
            system,
            user("make it punchy"),
            call,
            tool_result("tracks_mix", "{\"ok\":true}")
        ]);
        let prepared = store
            .prepare(
                &session.id,
                round.as_array().unwrap(),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        assert_eq!(prepared.appended, 1);
        assert_eq!(prepared.messages.len(), 3);
        assert_eq!(prepared.messages[2]["role"], "tool");

        // The next user turn rebuilds its own history from the panel's UI list.
        let final_answer = reply("Kick is at -3 dB.");
        store
            .record_reply(
                &session.id,
                &final_answer,
                &context,
                "fixture-provider",
                "fixture-model",
            )
            .unwrap();
        let next = json!([
            system,
            user("make it punchy"),
            reply("Kick is at -3 dB."),
            user("now the bass")
        ]);
        let prepared = store
            .prepare(
                &session.id,
                next.as_array().unwrap(),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        assert_eq!(prepared.appended, 1, "only the pending user message is new");
        assert_eq!(
            prepared.messages.last().unwrap(),
            &expected_user("now the bass")
        );
        assert_eq!(prepared.messages.len(), 5);

        // The same request twice appends nothing.
        let again = store
            .prepare(
                &session.id,
                next.as_array().unwrap(),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        assert_eq!(again.appended, 0);
        assert_eq!(again.messages.len(), 5);
        let loaded = store.load(&session.id).unwrap();
        assert_eq!(loaded.messages.len(), 5);
        // The saved transcript is the wire transcript: the tool row is there, in
        // order, with the call id its assistant turn used.
        assert_eq!(loaded.messages[1]["role"], "assistant");
        assert_eq!(loaded.messages[1]["tool_calls"][0]["id"], "call-tracks_mix");
        assert_eq!(loaded.messages[2]["role"], "tool");
        assert_eq!(loaded.messages[2]["tool_call_id"], "call-tracks_mix");
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn compaction_leaves_no_tool_call_state_or_signature_to_replay() {
        let (store, project) = fixture("compact");
        let session = store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                Some("punchy kick"),
                None,
            )
            .unwrap();
        let context = facts(&project);
        let view = conversation();
        let before = estimate_tokens(&view);
        store
            .prepare(
                &session.id,
                &view,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();

        let outcome = store.compact(&session.id, None, &context).unwrap();
        assert!(strips_replay_state(&outcome.messages));
        assert_eq!(outcome.dropped_from, 0);
        assert!(outcome.dropped_to > 0);
        assert!(outcome.tokens_after < outcome.tokens_before);
        assert_eq!(outcome.tokens_before, before);
        assert!(outcome.retained_pairs > 0);

        let text = outcome.messages[0]["content"].as_str().unwrap();
        assert!(text.contains("Retained recent tool calls"), "{text}");
        assert!(text.contains("tracks_mix → "), "{text}");
        assert!(text.contains("What was tried and failed"), "{text}");
        assert!(text.contains("track 'pad' not found"), "{text}");
        assert!(text.contains("Standing intent"), "{text}");
        assert!(
            text.contains(&dsl_digest("@project\n  tempo: 128\n")),
            "{text}"
        );
        assert!(text.contains("Dropped messages 0.."), "{text}");
        assert_eq!(outcome.messages[0]["role"], "user");
        assert_eq!(outcome.messages[0]["context"], "summary");
        // The pending user message survives: the model still has the question.
        assert_eq!(
            outcome.messages.last().unwrap(),
            &expected_user("now the bass")
        );

        let serialized = Value::Array(outcome.messages.clone()).to_string();
        for forbidden in [
            "tool_calls",
            "provider_state",
            "thought_signature",
            "signature",
            "encrypted_content",
            "extra_content",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "'{forbidden}' survived compaction: {serialized}"
            );
        }
        // The record states what it dropped and how the estimate moved. It is
        // appended after the turns it summarizes, which stay on disk.
        let dir = store.root().join(&session.workspace_key).join(&session.id);
        let contents = std::fs::read_to_string(dir.join(MESSAGES_FILE)).unwrap();
        let records: Vec<Value> = contents
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let record = records
            .iter()
            .find(|record| record["kind"] == "compaction")
            .expect("a compaction record");
        if std::env::var_os("OCP_SESSIONS_KEEP").is_some() {
            eprintln!("COMPACTION_RECORD {record}");
        }
        assert!(
            records.iter().any(|record| record["kind"] == "turn"),
            "the turns before the summary must stay on disk"
        );
        let (original, compactions, _) = read_original(&dir).unwrap();
        assert_eq!(compactions, 1);
        assert!(
            original.len() > outcome.messages.len(),
            "the original history is larger than the compacted view"
        );
        assert_eq!(record["kind"], "compaction");
        assert_eq!(record["dropped"]["from"], 0);
        assert_eq!(record["dropped"]["to"], outcome.dropped_to);
        assert_eq!(record["tokens_before"], outcome.tokens_before);
        assert_eq!(record["tokens_after"], outcome.tokens_after);
        assert_eq!(store.load(&session.id).unwrap().messages, outcome.messages);
        assert!(store.load(&session.id).unwrap().meta.compacted_at.is_some());
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn compaction_keeps_a_local_audio_reference_from_folded_history() {
        let reference = json!({
            "kind": "audio", "name": "song.wav", "path": "/tmp/song.wav", "mime": "audio/wav",
            "bytes": crate::ai::media::MAX_FILE_BYTES + 1, "duration_secs": 100.0,
            "sha256": "a".repeat(64), "local_reference": true,
        });
        let view = [
            json!({"role":"user", "content":"Use this as the reference", "media":[reference]}),
            json!({"role":"assistant", "content":"A detailed listening report. ".repeat(1000)}),
            json!({"role":"user", "content":"Continue"}),
            json!({"role":"assistant", "content":"Continuing."}),
        ];
        let outcome = plan_compaction(&view, 0, &ProjectFacts::default(), "fixture").unwrap();
        assert_eq!(outcome.messages[0]["media"][0], reference);
        let mut replay = outcome.messages;
        bound_media_replay(&mut replay, &[]);
        assert_eq!(replay[0]["media"][0], reference);
    }

    #[test]
    fn a_compacted_session_keeps_real_user_prose_and_bounds_auditions() {
        let (store, project) = fixture("auditions-bounded");
        let session = store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                Some("listen"),
                None,
            )
            .unwrap();
        let context = facts(&project);
        let descriptor = |index: usize| {
            json!({
                "kind": "audio",
                "name": format!("audition-{index}.wav"),
                "path": format!("/tmp/audition-media/audition-{index}.wav"),
                "mime": "audio/wav",
                "bytes": 1_000 + index as u64,
                "duration_secs": 8.0,
                "sha256": format!("{:064x}", index + 1),
            })
        };
        let mut view = vec![user("make the kick punchier")];
        for index in 0..34usize {
            view.push(tool_call(
                "audition_range",
                &format!("{{\"start_bar\":{},\"end_bar\":{}}}", index + 1, index + 2),
                &format!("sig-{index}"),
            ));
            view.push(tool_result(
                "audition_range",
                &json!({"ok": true, "range": index}).to_string(),
            ));
            view.push(json!({
                "role": "user",
                "content": if index == 33 {
                    "A/B reference excerpt. Verified playback match.".into()
                } else {
                    format!("You asked to hear {index}–{} s: this is that render.", index + 1)
                },
                "context": "audition",
                "media": [descriptor(index)],
            }));
            if index == 32 {
                view.last_mut().unwrap()["source_render"] = json!(
                    store
                        .root()
                        .join("audition-renders")
                        .join(format!("{}.wav", "a".repeat(64)))
                );
            }
            if index == 33 {
                view.push(json!({
                    "role": "user",
                    "content": "A/B candidate excerpt. Verified playback match.",
                    "context": "audition",
                    "media": [descriptor(34)],
                }));
            }
            if index % 7 == 0 {
                view.push(user(&format!("real ask {index}")));
            }
            view.push(reply(&format!("round {index} done")));
        }
        view.push(user("now the bass"));
        let dir = store.find(&session.id).unwrap().0;
        // A version-one archive predates explicit message metadata. Reading it
        // migrates the saved labels once; new writers never infer these kinds.
        let mut legacy = turn_record("2026-11-03T08:00:00Z", view);
        for message in &mut legacy.messages {
            message.as_object_mut().unwrap().remove("message_meta");
        }
        append_records(&dir, &[legacy]).unwrap();

        let outcome = store.compact(&session.id, None, &context).unwrap();
        assert!(strips_replay_state(&outcome.messages));
        assert!(outcome.retained_pairs <= RETAINED_TOOL_PAIRS);

        // The summary is marked as host context; it is not a user turn.
        let summaries: Vec<&Value> = outcome
            .messages
            .iter()
            .filter(|message| message["context"] == CONTEXT_SUMMARY)
            .collect();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0]["role"], "user");

        // Regular auditions share a row per round; A and B keep their labels.
        let auditions: Vec<&Value> = outcome
            .messages
            .iter()
            .filter(|message| message["context"] == CONTEXT_AUDITION)
            .collect();
        assert!(!auditions.is_empty());
        assert!(
            auditions.len() <= RETAINED_TOOL_PAIRS + 1,
            "{} audition rows",
            auditions.len()
        );
        for row in &auditions {
            assert_eq!(row["role"], "user");
            assert!(
                row["media"]
                    .as_array()
                    .is_some_and(|media| !media.is_empty())
            );
            assert!(
                row["content"]
                    .as_str()
                    .is_some_and(|content| content.starts_with("You asked to hear ")
                        || content.starts_with("A/B "))
            );
        }
        let paired = retained_comparison_pair(&outcome.messages).unwrap();
        assert_eq!(paired[0]["media"][0]["name"], "audition-33.wav");
        assert_eq!(paired[1]["media"][0]["name"], "audition-34.wav");
        assert!(
            auditions
                .iter()
                .any(|row| audition_render_reference(row).is_some())
        );

        // Every other user row is a real turn the person wrote.
        let prose: Vec<&str> = outcome
            .messages
            .iter()
            .filter(|message| message["role"] == "user" && message.get("context").is_none())
            .filter_map(|message| message["content"].as_str())
            .collect();
        assert!(!prose.is_empty());
        for content in &prose {
            assert!(
                content.starts_with("real ask ") || *content == "now the bass",
                "{content}"
            );
        }

        let summary = summaries[0]["content"].as_str().unwrap();
        assert!(
            summary.contains("Discarded audition deliveries: 28 clips"),
            "{summary}"
        );
        assert!(
            summary.contains("Audio kept on disk: 7 clip(s)"),
            "{summary}"
        );
        assert!(
            !summary.contains("Earlier user asks:\n- \"You asked to hear"),
            "{summary}"
        );
        let serialized = Value::Array(outcome.messages.clone()).to_string();
        assert!(!serialized.contains("tool_calls"), "{serialized}");
        assert!(!serialized.contains("provider_state"), "{serialized}");
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    /// The operator's stored session, copied outside the repository. Run it
    /// with `OCP_CTX_ROWS_SESSION=<copy>/messages.jsonl --ignored`.
    #[test]
    #[ignore = "needs OCP_CTX_ROWS_SESSION=<copy of messages.jsonl>"]
    fn the_operator_session_compacts_to_bounded_context_and_real_prose() {
        let path = std::env::var("OCP_CTX_ROWS_SESSION")
            .expect("set OCP_CTX_ROWS_SESSION to the copied messages.jsonl");
        let text = std::fs::read_to_string(path).unwrap();
        let mut view = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let record: Record = serde_json::from_str(line).unwrap();
            if record.kind == RecordKind::Turn {
                view.extend(record.messages);
            }
        }
        canonicalize_media(&mut view);
        let outcome = plan_compaction(&view, 0, &ProjectFacts::default(), "operator").unwrap();
        assert!(strips_replay_state(&outcome.messages));

        let summaries: Vec<&Value> = outcome
            .messages
            .iter()
            .filter(|message| message["context"] == CONTEXT_SUMMARY)
            .collect();
        assert_eq!(summaries.len(), 1);
        let summary = summaries[0]["content"].as_str().unwrap();
        assert!(
            summary.contains("Discarded audition deliveries: "),
            "{summary}"
        );

        let auditions = outcome
            .messages
            .iter()
            .filter(|message| message["context"] == CONTEXT_AUDITION)
            .count();
        assert!(
            auditions <= RETAINED_TOOL_PAIRS,
            "{auditions} audition rows"
        );

        for message in &outcome.messages {
            if message["role"] != "user" || message.get("context").is_some() {
                continue;
            }
            let content = message["content"].as_str().unwrap_or_default();
            assert!(
                !content.starts_with("You asked to hear ") && !is_round_nudge(content),
                "{content}"
            );
        }
        let serialized = Value::Array(outcome.messages.clone()).to_string();
        assert!(!serialized.contains("tool_calls"), "{serialized}");
        assert!(!serialized.contains("provider_state"), "{serialized}");
    }

    #[test]
    fn renaming_a_session_marks_it_named() {
        let (store, project) = fixture("named");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        assert!(!session.named);
        let renamed = store.rename(&session.id, "Punchier kick").unwrap();
        assert!(renamed.named);
        assert_eq!(renamed.title, "Punchier kick");
        assert!(store.load(&session.id).unwrap().meta.named);
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_session_permission_round_trips_through_its_metadata() {
        let (store, project) = fixture("permission-roundtrip");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        assert_eq!(session.permission, SessionPermission::Write);

        let saved = store
            .set_permission(&session.id, SessionPermission::Autoreview)
            .unwrap();
        assert_eq!(saved.permission, SessionPermission::Autoreview);
        assert_eq!(
            store.load(&session.id).unwrap().meta.permission,
            SessionPermission::Autoreview
        );
        // The list carries the same value, and the file spells it on the wire.
        assert_eq!(
            store
                .list(Scope::Workspace(Some(&workspace(&project))))
                .unwrap()[0]
                .permission,
            SessionPermission::Autoreview
        );
        let dir = store.find(&session.id).unwrap().0;
        let record: Value =
            serde_json::from_slice(&std::fs::read(dir.join(META_FILE)).unwrap()).unwrap();
        assert_eq!(record["permission"], "autoreview");
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn an_unknown_session_permission_is_refused() {
        // The wire boundary is the enum: a value outside the three modes does
        // not deserialize, so it cannot reach the store.
        for value in ["everything", "", "WRITE", "autoreviews"] {
            assert!(
                serde_json::from_value::<SessionPermission>(json!(value)).is_err(),
                "'{value}' was accepted"
            );
        }
        for (permission, wire) in [
            (SessionPermission::RequireApproval, "require_approval"),
            (SessionPermission::Autoreview, "autoreview"),
            (SessionPermission::Write, "write"),
        ] {
            assert_eq!(
                serde_json::from_value::<SessionPermission>(json!(wire)).unwrap(),
                permission
            );
            assert_eq!(serde_json::to_value(permission).unwrap(), json!(wire));
        }
    }

    #[test]
    fn a_session_saved_before_the_permission_field_reads_the_default() {
        let (store, project) = fixture("permission-legacy");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let dir = store.find(&session.id).unwrap().0;
        // A metadata file written by a build that had no permission field.
        let mut record: Value =
            serde_json::from_slice(&std::fs::read(dir.join(META_FILE)).unwrap()).unwrap();
        record.as_object_mut().unwrap().remove("permission");
        std::fs::write(
            dir.join(META_FILE),
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
        assert_eq!(
            store.load(&session.id).unwrap().meta.permission,
            SessionPermission::Write
        );
    }

    #[test]
    fn two_sessions_keep_independent_permissions() {
        let (store, project) = fixture("permission-independent");
        let place = workspace(&project);
        let first = store
            .create(Some(&place), Some(&project), Some("first"), None)
            .unwrap();
        let second = store
            .create(Some(&place), Some(&project), Some("second"), None)
            .unwrap();
        store
            .set_permission(&first.id, SessionPermission::RequireApproval)
            .unwrap();
        store
            .set_permission(&second.id, SessionPermission::Write)
            .unwrap();

        // A reopened store reads the same two values, one per session.
        let reopened = Store::new(store.root().to_owned());
        assert_eq!(
            reopened.load(&first.id).unwrap().meta.permission,
            SessionPermission::RequireApproval
        );
        assert_eq!(
            reopened.load(&second.id).unwrap().meta.permission,
            SessionPermission::Write
        );
        // Changing one leaves the other alone.
        reopened
            .set_permission(&second.id, SessionPermission::Autoreview)
            .unwrap();
        assert_eq!(
            reopened.load(&first.id).unwrap().meta.permission,
            SessionPermission::RequireApproval
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_rewind_keeps_the_prefix_and_closes_a_dangling_call() {
        let (store, project) = fixture("rewind");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let (dir, _) = store.find(&session.id).unwrap();
        let call = json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "call-1", "type": "function",
                            "function": {"name": "edit_subtree", "arguments": "{}"}}]
        });
        let record = Record {
            v: VERSION,
            kind: RecordKind::Turn,
            at: now(),
            dropped: None,
            tokens_before: None,
            tokens_after: None,
            truncated: None,
            messages: vec![user("first"), call, user("outcome")],
        };
        write_records(&dir, &[record]).unwrap();

        let kept = store.rewind(&session.id, 2).unwrap();
        assert_eq!(kept, 3, "the kept prefix plus the closed call");
        let (view, _) = read_view(&dir).unwrap();
        assert_eq!(view[0]["content"], "first");
        assert_eq!(view[1]["tool_calls"][0]["id"], "call-1");
        // The call the cut left open is answered, so the next request is valid.
        assert_eq!(view[2]["role"], "tool");
        assert_eq!(view[2]["tool_call_id"], "call-1");
        assert_eq!(view.len(), 3, "a completed call gets no second result");
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    /// One round as the loop saves it: the call and the result it produced.
    fn completed_round(round: usize) -> Vec<Value> {
        vec![
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": format!("done-{round}"),
                    "type": "function",
                    "function": {"name": "tracks_list", "arguments": "{}"}
                }]
            }),
            json!({
                "role": "tool",
                "tool_call_id": format!("done-{round}"),
                "content": "{\"ok\":true}"
            }),
        ]
    }

    #[test]
    fn a_rewind_closes_only_the_call_it_left_open() {
        let (store, project) = fixture("rewind-completed");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let (dir, _) = store.find(&session.id).unwrap();
        let mut messages = vec![user("three rounds, then one call still open")];
        for round in 0..3 {
            messages.extend(completed_round(round));
        }
        messages.push(json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "open",
                "type": "function",
                "function": {"name": "tracks_add", "arguments": "{}"}
            }]
        }));
        write_records(&dir, &[turn_record(&now(), messages)]).unwrap();

        // Keeping every message drops nothing, so the trailing call is the only
        // one without a result.
        let kept = store.rewind(&session.id, 8).unwrap();
        let (view, _) = read_view(&dir).unwrap();
        assert_eq!(kept, 9, "the kept prefix plus one closing result");
        let closed: Vec<&str> = view
            .iter()
            .filter(|message| message["tool_call_id"].is_string())
            .filter(|message| {
                message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("rewound before"))
            })
            .filter_map(|message| message["tool_call_id"].as_str())
            .collect();
        assert_eq!(closed, ["open"], "completed calls are not answered again");
        let mut answered: Vec<&str> = view
            .iter()
            .filter_map(|message| message["tool_call_id"].as_str())
            .collect();
        answered.sort_unstable();
        assert_eq!(
            answered,
            ["done-0", "done-1", "done-2", "open"],
            "no call is answered twice"
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_rewind_lands_on_the_start_of_a_tool_block() {
        let (store, project) = fixture("rewind-block");
        let block = || {
            vec![
                user("start"),
                json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {"id": "call-a", "type": "function",
                         "function": {"name": "tracks_list", "arguments": "{}"}},
                        {"id": "call-b", "type": "function",
                         "function": {"name": "buses_list", "arguments": "{}"}}
                    ]
                }),
                json!({"role": "tool", "tool_call_id": "call-a", "content": "{\"ok\":true}"}),
                json!({"role": "tool", "tool_call_id": "call-b", "content": "{\"ok\":true}"}),
                user("outcome"),
            ]
        };
        for (label, cut, expected) in [
            // A cut on either result moves back to the call that opened the block.
            ("first-result", 2, 1),
            ("second-result", 3, 1),
            // A cut after the block keeps it whole.
            ("after-block", 5, 5),
        ] {
            let session = store
                .create(
                    Some(&workspace(&project)),
                    Some(&project),
                    Some(label),
                    None,
                )
                .unwrap();
            let (dir, _) = store.find(&session.id).unwrap();
            write_records(&dir, &[turn_record(&now(), block())]).unwrap();
            assert_eq!(
                store.rewind(&session.id, cut).unwrap(),
                expected,
                "{label}: keep {cut}"
            );
            let (view, _) = read_view(&dir).unwrap();
            assert_eq!(view.len(), expected, "{label}");
            assert_eq!(view[0]["content"], "start");
            if expected == 1 {
                assert_eq!(view[0]["role"], "user", "{label}: the whole block is gone");
            }
        }
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_rewind_refuses_an_index_outside_the_conversation() {
        let (store, project) = fixture("rewind-range");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let (dir, _) = store.find(&session.id).unwrap();
        write_records(&dir, &[turn_record(&now(), vec![user("only message")])]).unwrap();

        assert!(
            store.rewind(&session.id, 0).is_err(),
            "nothing would be kept"
        );
        assert!(store.rewind(&session.id, 2).is_err(), "past the end");
        assert_eq!(
            store.view(&session.id).unwrap().len(),
            1,
            "a refused rewind leaves the conversation alone"
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_saved_snapshot_survives_a_reload_and_a_sweep() {
        let (store, project) = fixture("saved-snapshot");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let (dir, _) = store.find(&session.id).unwrap();
        let kept_sha = store
            .snapshot_dsl(&session.id, "@project \"kept\"\n")
            .unwrap();
        let orphan_sha = store
            .snapshot_dsl(&session.id, "@project \"orphan\"\n")
            .unwrap();
        let history = vec![json!({
            "role": "user",
            "content": "keep this take",
            "dsl_sha": kept_sha,
        })];
        store
            .prepare(
                &session.id,
                &history,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 1_000_000,
                    facts: &facts(&project),
                    provider: "fixture",
                    model: "model",
                },
            )
            .unwrap();

        // A reopened conversation carries the snapshot its row reverts to.
        let loaded = store.load(&session.id).unwrap();
        let saved = loaded
            .messages
            .iter()
            .find(|message| message["content"] == "keep this take")
            .expect("the user message was saved");
        assert_eq!(saved["dsl_sha"], json!(kept_sha));

        // The sweep reads the saved messages, so a saved snapshot is a reference
        // and an unreferenced version is still collected.
        assert_eq!(store.collect_unreferenced().unwrap(), 1);
        assert!(
            dir.join("versions")
                .join(format!("{kept_sha}.ocp"))
                .is_file()
        );
        assert!(
            !dir.join("versions")
                .join(format!("{orphan_sha}.ocp"))
                .is_file()
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    fn add_padded_wav_metadata(wav: &mut Vec<u8>) {
        wav.splice(12..12, *b"JUNK\x03\0\0\0abc\0");
        let riff_size = (wav.len() as u32 - 8).to_le_bytes();
        wav[4..8].copy_from_slice(&riff_size);
    }

    #[test]
    fn a_stereo_render_is_archived_beside_the_clip_it_produced() {
        use crate::gui_settings::{SessionAudioFormat, SessionAudioSettings};
        use base64::Engine as _;
        let (store, _project) = fixture("audition-stereo");
        let root = store.root().to_path_buf();
        std::fs::create_dir_all(root.join("audition-media")).unwrap();

        // A 48 kHz stereo render, as the audition path writes it.
        let frames = 4_800usize;
        let mut pcm = Vec::with_capacity(frames * 4);
        for frame in 0..frames {
            let value = ((frame as f32) * 0.05).sin();
            let sample = (value * 12_000.0) as i16;
            pcm.extend_from_slice(&sample.to_le_bytes());
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(&pcm);
        add_padded_wav_metadata(&mut wav);
        let source = root.join("stereo-source.wav");
        std::fs::write(&source, &wav).unwrap();

        // The delivered clip is the mono 16 kHz copy; the message carries the
        // render it came from.
        let mono: Vec<f32> = (0..1_600).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let clip = oc_engine::wav::mono_wav_bytes(&mono, 16_000).unwrap();
        let mut messages = vec![json!({
            "role": "user",
            "content": "You asked to hear 0-8 s: this is that render.",
            "message_meta": {"version":1,"kind":"audition"}, "input_audio": {
                "data": base64::engine::general_purpose::STANDARD.encode(&clip),
                "format": "wav",
                "source_path": source.display().to_string(),
            }
        })];
        let evicted = persist_auditions(
            &root,
            &mut messages,
            &SessionAudioSettings {
                format: SessionAudioFormat::Wav,
                mp3_kbps: 128,
            },
        )
        .unwrap();
        assert!(evicted.is_empty(), "a small archive evicts nothing");
        // The delivery is the mono 16 kHz clip the model heard, byte for byte.
        let media = messages[0]["media"].as_array().expect("archived media");
        assert_eq!(media.len(), 1, "one delivery attachment");
        let path = media[0]["path"].as_str().expect("media path");
        assert_eq!(
            std::fs::read(path).unwrap(),
            clip,
            "the wire points at the clip's own bytes"
        );
        let decoded = oc_engine::audio_clip::decode(Path::new(path)).unwrap();
        assert_eq!(
            decoded.sample_rate, 16_000,
            "the delivery keeps the clip's rate"
        );
        // The full-rate render stays on disk for the operator, and no request
        // resolves it: it is recorded as a path, not as a media descriptor.
        let render = messages[0]["source_render"]
            .as_str()
            .expect("the operator's render is recorded");
        assert!(render.contains("audition-renders"), "{render}");
        let render = oc_engine::audio_clip::decode(Path::new(render)).unwrap();
        assert_eq!(
            render.sample_rate, 44_100,
            "the render archive is a 44.1 kHz file"
        );
        let _ = std::fs::remove_file(&source);
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    /// A 120 s stereo render far above the inline limit, delivered as the
    /// bounded mono 16 kHz clip the listening tool produced.
    #[test]
    fn an_audition_replays_its_bounded_clip_not_the_full_rate_render() {
        use crate::gui_settings::{SessionAudioFormat, SessionAudioSettings};
        use base64::Engine as _;
        let (store, project) = fixture("audition-bound");
        let root = store.root().to_path_buf();
        let place = workspace(&project);
        let session = store
            .create(Some(&place), Some(&project), Some("listen"), None)
            .unwrap();
        let context = facts(&project);
        let settings = SessionAudioSettings {
            format: SessionAudioFormat::Wav,
            mp3_kbps: 128,
        };

        // 120 s of 44.1 kHz stereo: 21 MB, well over the 12 MiB inline limit.
        let frames = 120 * 44_100;
        let mut pcm = Vec::with_capacity(frames * 4);
        for frame in 0..frames {
            let sample = (((frame as f32) * 0.001).sin() * 9_000.0) as i16;
            pcm.extend_from_slice(&sample.to_le_bytes());
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
        let mut wav = Vec::with_capacity(pcm.len() + 44);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&44_100u32.to_le_bytes());
        wav.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(&pcm);
        assert!(wav.len() as u64 > crate::ai::media::MAX_FILE_BYTES);
        let source = place.join("render.wav");
        std::fs::write(&source, &wav).unwrap();

        // What the listening tool sent: the first 30 s of that window, mono 16 kHz.
        let mono: Vec<f32> = (0..30 * 16_000)
            .map(|index| (index as f32 * 0.001).sin() * 0.3)
            .collect();
        let clip = oc_engine::wav::mono_wav_bytes(&mono, 16_000).unwrap();
        let mut messages = vec![json!({
            "role": "user",
            "content": "You asked to hear 0–120 s: this is that render.",
            "message_meta": {"version":1,"kind":"audition"}, "input_audio": {
                "data": base64::engine::general_purpose::STANDARD.encode(&clip),
                "format": "wav",
                "source_path": source.display().to_string(),
            }
        })];
        let evicted = persist_auditions(&root, &mut messages, &settings).unwrap();
        assert!(evicted.is_empty(), "a small archive evicts nothing");

        // The archive keeps both: the bounded delivery a request reads, and the
        // full-rate render only the operator opens.
        let descriptor = messages[0]["media"][0].clone();
        assert_eq!(
            std::fs::read(descriptor["path"].as_str().unwrap()).unwrap(),
            clip,
            "the wire points at the clip's own bytes"
        );
        assert!(
            descriptor["bytes"].as_u64().unwrap() <= crate::ai::media::MAX_FILE_BYTES,
            "the delivery is within the inline bound: {descriptor}"
        );
        let render = messages[0]["source_render"]
            .as_str()
            .expect("the operator's render is recorded");
        assert!(render.contains("audition-renders"), "{render}");
        assert!(std::fs::metadata(render).unwrap().len() > crate::ai::media::MAX_FILE_BYTES);
        assert_eq!(
            std::fs::read(render).unwrap(),
            wav,
            "archive preserves the original WAV bytes"
        );

        // Save it, then reload: the session is what a later request reads.
        store
            .prepare(
                &session.id,
                &messages,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        let reopened = Store::new(root.clone());
        let loaded = reopened.load(&session.id).unwrap();
        let saved = loaded
            .messages
            .iter()
            .find(|message| message["context"] == CONTEXT_AUDITION)
            .expect("the audition is saved");
        assert!(
            saved.get("input_audio").is_none(),
            "the inline bytes are replaced by the archive"
        );
        assert_eq!(saved["media"][0], descriptor);
        assert_eq!(saved["source_render"], json!(render));
        let saved_index = loaded
            .messages
            .iter()
            .position(|message| message == saved)
            .unwrap();
        let retained = retained_media_row(&loaded.messages, saved_index, saved_index + 1).unwrap();
        assert_eq!(retained["source_render"], json!(render));

        // The request a later turn builds carries the clip's bytes and none of
        // the render's.
        let mut outbound = loaded.messages.clone();
        describe_audition_renders(&root, &mut outbound);
        let mut request = ai::ProviderRequest {
            system: None,
            turns: ai::turns_from_wire(&Value::Array(outbound)).unwrap(),
            tools: Vec::new(),
            max_tokens: 1024,
            temperature: 0.0,
            thinking_level: "medium".into(),
            stream: false,
        };
        let notices = ai::media::resolve(&mut request).unwrap();
        assert!(notices.is_empty(), "the archived clip did not change");
        assert!(request.turns.iter().flat_map(|turn| &turn.blocks).any(|block| {
            matches!(block, ai::Block::Text(text) if text.contains("audition_clip_source") && text.contains(render))
        }));
        let data = request
            .turns
            .iter()
            .flat_map(|turn| &turn.blocks)
            .find_map(|block| match block {
                ai::Block::Media { data, .. } => Some(data),
                _ => None,
            })
            .expect("the delivery is sent");
        let ai::media::MediaData::Inline { base64, source } = data else {
            panic!("the delivery was not resolved");
        };
        let body = base64::engine::general_purpose::STANDARD
            .decode(base64)
            .unwrap();
        assert!(body.len() as u64 <= crate::ai::media::MAX_FILE_BYTES);
        assert_eq!(body, clip, "the request carries the clip, not the render");
        assert_eq!(source.bytes, clip.len() as u64);

        // The clip's own bounds: mono, 16 kHz, 30 s. The render's 120 s and two
        // channels never reach the wire.
        let find = |needle: &[u8]| body.windows(4).position(|window| window == needle);
        let fmt = find(b"fmt ").expect("a format chunk");
        let channels = u16::from_le_bytes([body[fmt + 10], body[fmt + 11]]);
        let rate = u32::from_le_bytes(body[fmt + 12..fmt + 16].try_into().unwrap());
        let data = find(b"data").expect("a data chunk");
        let data_len = u32::from_le_bytes(body[data + 4..data + 8].try_into().unwrap());
        let seconds = f64::from(data_len) / f64::from(rate) / f64::from(channels) / 2.0;
        assert_eq!(channels, 1, "the delivery stays mono");
        assert_eq!(rate, 16_000, "the delivery stays at the clip rate");
        assert!(
            (seconds - 30.0).abs() < 0.01,
            "the clip's duration, not the render's: {seconds} s"
        );

        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    /// A sparse file of `bytes`: the budget reads its length, and writing 300
    /// MiB of real bytes would make the test slower than the sweep it checks.
    fn sparse(path: &Path, bytes: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::File::create(path).unwrap().set_len(bytes).unwrap();
    }

    /// A short valid WAV a turn can inline as its audition clip.
    fn audition_clip() -> String {
        let samples: Vec<f32> = (0..1_600)
            .map(|index| (index as f32 * 0.05).sin() * 0.2)
            .collect();
        let wav = oc_engine::wav::mono_wav_bytes(&samples, 16_000).unwrap();
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(&wav)
    }

    #[test]
    fn audition_collection_waits_for_transcript_publication_in_another_process() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        const CHILD_ROOT: &str = "OCP_TEST_AUDITION_COLLECT_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) {
            let error = private_fs::lock(&root.join(".sessions.lock"), false, true).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
            std::fs::write(root.join("collector-ready"), []).unwrap();
            assert_eq!(Store::new(root).collect_unreferenced().unwrap(), 1);
            return;
        }

        let store = Store::new(oc_audio::private_fs::test_root().join("audition-collector-lock"));
        let session = store.create(None, None, None, None).unwrap();
        let (dir, _) = store.find(&session.id).unwrap();
        let lock = store.mutation_lock().unwrap();
        let mut messages = vec![json!({
            "role": "user", "content": "The audition",
            "message_meta": {"version":1,"kind":"audition"}, "input_audio": {"data": audition_clip(), "format": "wav"},
        })];
        persist_auditions(store.root(), &mut messages, &Default::default()).unwrap();
        let clip = std::fs::read_dir(clip_dir(store.root()))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let orphan = render_dir(store.root()).join(format!("{}.wav", "a".repeat(64)));
        sparse(&orphan, 32);

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ai::sessions::tests::audition_collection_waits_for_transcript_publication_in_another_process",
            ])
            .env(CHILD_ROOT, store.root())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let ready = store.root().join("collector-ready");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.is_file() && child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let published = ready.is_file() && clip.is_file();
        let result = append_records(&dir, &[turn_record(&now(), messages)]);
        drop(lock);
        if !published {
            let _ = child.kill();
        }
        let status = child.wait().unwrap();
        result.unwrap();
        assert!(published && status.success());
        assert!(clip.is_file());
        assert!(!orphan.exists());
    }

    #[test]
    fn a_truncated_wav_does_not_evict_for_the_declared_frame_count() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let source = root.join("source.wav");
        let rate = 48_000;
        let frames = 20_000;
        let mut bytes = oc_engine::wav::pcm_wav_bytes(&vec![0.3; frames * 2], rate, 2).unwrap();
        bytes.truncate(bytes.len() - 4);
        std::fs::write(&source, bytes).unwrap();
        let decoded = oc_engine::audio_clip::decode(&source).unwrap();
        assert!(decoded.frames() < frames);
        let quality = oc_engine::resample::ResampleQuality::DEFAULT_RENDER;
        let resampled =
            oc_engine::resample::resample(&decoded.samples, rate, 44_100, quality).unwrap();
        let expected = oc_engine::wav::pcm_wav_bytes(&resampled, 44_100, 2).unwrap();

        let old = render_dir(root).join(format!("{}.wav", "a".repeat(64)));
        sparse(&old, AUDITION_ARCHIVE_BUDGET - expected.len() as u64);
        let store = Store::new(root.to_path_buf());
        let _lock = store.mutation_lock().unwrap();
        let archived = ArchiveSweep::new(root)
            .render(&source, &Default::default())
            .unwrap();
        assert_eq!(std::fs::read(archived).unwrap(), expected);
        assert!(
            old.is_file(),
            "the real conversion fits beside the existing archive"
        );
    }

    #[test]
    fn audition_archive_budgets_the_resampled_wav_size() {
        let root = oc_audio::private_fs::test_root().join("audition-wav-budget");
        let old = render_dir(&root).join(format!("{}.wav", "b".repeat(64)));
        sparse(&old, AUDITION_ARCHIVE_BUDGET - 9_000);
        let source = root.join("source.wav");
        let bytes = oc_engine::wav::pcm_wav_bytes(&vec![0.0; 3_200], 16_000, 2).unwrap();
        assert!(bytes.len() < 9_000);
        std::fs::write(&source, bytes).unwrap();
        let store = Store::new(root.clone());
        let _lock = store.mutation_lock().unwrap();
        let output = ArchiveSweep::new(&root)
            .render(&source, &Default::default())
            .unwrap();
        assert!(output.metadata().unwrap().len() > 9_000);
        assert!(!old.exists());
        assert!(archived_bytes(&root) <= AUDITION_ARCHIVE_BUDGET);
    }

    #[test]
    fn the_sweep_collects_audition_renders_and_keeps_referenced_files() {
        let (store, project) = fixture("audition-renders-swept");
        let root = store.root().to_path_buf();
        let session = store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                Some("archive"),
                None,
            )
            .unwrap();
        let (dir, _) = store.find(&session.id).unwrap();
        let kept_clip = "a".repeat(64);
        let kept_render = "b".repeat(64);
        write_records(
            &dir,
            &[turn_record(
                &now(),
                vec![json!({
                    "role": "user",
                    "content": format!("clip {kept_clip}, render {kept_render}"),
                    "context": "audition",
                    "source_render": root
                        .join("audition-renders")
                        .join(format!("{kept_render}.wav"))
                        .display()
                        .to_string(),
                })],
            )],
        )
        .unwrap();
        for (dir_name, hash, bytes) in [
            ("audition-media", kept_clip.clone(), 4_096),
            ("audition-renders", kept_render.clone(), 8_192),
            ("audition-media", "c".repeat(64), 100 * 1024 * 1024),
            ("audition-renders", "d".repeat(64), 200 * 1024 * 1024),
        ] {
            sparse(&root.join(dir_name).join(format!("{hash}.wav")), bytes);
        }

        let before = audition_archive(&root);
        assert_eq!(before.media.files, 2);
        assert_eq!(before.renders.files, 2);
        assert!(
            before.media.bytes + before.renders.bytes > before.budget_bytes,
            "the fixture is over budget"
        );

        // Only the two files no message names are collected: a render is swept
        // exactly like a clip, and both referenced files survive.
        assert_eq!(store.collect_unreferenced().unwrap(), 2);
        let after = audition_archive(&root);
        assert_eq!(after.media.files, 1);
        assert_eq!(after.renders.files, 1);
        assert!(after.media.bytes + after.renders.bytes < after.budget_bytes);
        assert!(
            root.join("audition-media")
                .join(format!("{kept_clip}.wav"))
                .is_file()
        );
        assert!(
            root.join("audition-renders")
                .join(format!("{kept_render}.wav"))
                .is_file()
        );
        assert!(
            !root
                .join("audition-media")
                .join(format!("{}.wav", "c".repeat(64)))
                .exists()
        );
        assert!(
            !root
                .join("audition-renders")
                .join(format!("{}.wav", "d".repeat(64)))
                .exists()
        );

        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_full_archive_evicts_the_oldest_unreferenced_file_for_a_new_audition() {
        let (store, project) = fixture("audition-archive-full");
        let root = store.root().to_path_buf();
        let session = store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                Some("full"),
                None,
            )
            .unwrap();
        let (dir, _) = store.find(&session.id).unwrap();
        // A clip a live conversation names must survive the eviction.
        let kept_clip = "a".repeat(64);
        write_records(
            &dir,
            &[turn_record(
                &now(),
                vec![json!({
                    "role": "user",
                    "content": format!("listen to {kept_clip}"),
                    "context": "audition",
                })],
            )],
        )
        .unwrap();
        sparse(
            &root.join("audition-media").join(format!("{kept_clip}.wav")),
            1_024,
        );
        // The oldest file in the archive is an unreferenced render; 200 MiB of
        // unreferenced clips follow it, so the archive is full and both
        // directories count against the one budget.
        let oldest_render = "f".repeat(64);
        sparse(
            &root
                .join("audition-renders")
                .join(format!("{oldest_render}.wav")),
            100 * 1024 * 1024,
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        sparse(
            &root
                .join("audition-media")
                .join(format!("{}.wav", "b".repeat(64))),
            100 * 1024 * 1024,
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        sparse(
            &root
                .join("audition-media")
                .join(format!("{}.wav", "c".repeat(64))),
            100 * 1024 * 1024,
        );

        let incoming = vec![json!({
            "role": "user",
            "content": "listen",
            "message_meta": {"version":1,"kind":"audition"}, "input_audio": { "data": audition_clip(), "format": "wav" },
        })];
        let prepared = store
            .prepare(
                &session.id,
                &incoming,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 1_000_000,
                    facts: &facts(&project),
                    provider: "fixture",
                    model: "model",
                },
            )
            .expect("a full archive does not refuse the turn");

        // The turn result names what it removed, oldest first.
        assert_eq!(
            prepared.evicted_auditions.len(),
            1,
            "{:?}",
            prepared.evicted_auditions
        );
        assert_eq!(prepared.evicted_auditions[0].dir, "audition-renders");
        assert_eq!(
            prepared.evicted_auditions[0].file,
            format!("{oldest_render}.wav")
        );
        assert!(
            !root
                .join("audition-renders")
                .join(format!("{oldest_render}.wav"))
                .exists()
        );
        assert!(
            root.join("audition-media")
                .join(format!("{kept_clip}.wav"))
                .is_file()
        );

        // The new clip is archived, and the saved turn still carries it.
        let saved = prepared.messages.last().expect("the turn is saved");
        assert_eq!(saved["context"], "audition");
        let path = saved["media"][0]["path"]
            .as_str()
            .expect("the delivery names its clip");
        assert!(Path::new(path).is_file(), "{path}");

        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn an_audition_render_reference_survives_its_file_and_reports_missing() {
        let (store, project) = fixture("audition-render-missing");
        let root = store.root().to_path_buf();
        let session = store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                Some("gone"),
                None,
            )
            .unwrap();
        let (dir, _) = store.find(&session.id).unwrap();
        let render = root
            .join("audition-renders")
            .join(format!("{}.wav", "e".repeat(64)));
        std::fs::create_dir_all(render.parent().unwrap()).unwrap();
        std::fs::write(&render, vec![0u8; 128]).unwrap();
        write_records(
            &dir,
            &[turn_record(
                &now(),
                vec![json!({
                    "role": "user",
                    "content": "listen",
                    "context": "audition",
                    "source_render": render.display().to_string(),
                })],
            )],
        )
        .unwrap();

        // While the render is there the row carries no missing flag.
        let loaded = store.load(&session.id).unwrap();
        assert!(loaded.messages[0].get("source_render_missing").is_none());

        std::fs::remove_file(&render).unwrap();
        let loaded = store.load(&session.id).unwrap();
        assert_eq!(
            loaded.messages[0]["source_render"],
            json!(render.display().to_string()),
            "the descriptor keeps naming the render"
        );
        assert_eq!(loaded.messages[0]["source_render_missing"], json!(true));

        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn clearing_the_audition_archive_removes_both_directories() {
        let (store, _project) = fixture("audition-clear");
        let root = store.root().to_path_buf();
        for (dir_name, hash) in [
            ("audition-media", "a".repeat(64)),
            ("audition-renders", "b".repeat(64)),
        ] {
            sparse(&root.join(dir_name).join(format!("{hash}.wav")), 1_024);
        }

        assert_eq!(store.clear_audition_archive().unwrap(), 2);
        let archive = audition_archive(&root);
        assert_eq!(archive.media.files, 0);
        assert_eq!(archive.renders.files, 0);
        assert_eq!(archive.budget_bytes, AUDITION_ARCHIVE_BUDGET);

        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn an_mp3_bitrate_is_clamped_to_the_rate_table() {
        // MPEG-2 (24 kHz) writes at most 160 kbps; MPEG-1 carries 192.
        assert_eq!(mp3_bitrate(24_000, 192), 160);
        assert_eq!(mp3_bitrate(24_000, 320), 160);
        assert_eq!(mp3_bitrate(48_000, 192), 192);
        assert_eq!(mp3_bitrate(48_000, 64), 64);
    }

    #[test]
    fn missing_full_audition_render_keeps_the_excerpt_and_records_the_warning() {
        let (store, project) = fixture("audition-missing-full");
        let root = store.root().to_path_buf();
        let place = workspace(&project);
        let session = store
            .create(Some(&place), Some(&project), None, None)
            .unwrap();
        let incoming = vec![json!({
            "role": "user",
            "content": "You asked to hear 0–1 s: this is that render.",
            "message_meta": {"version":1,"kind":"audition"}, "input_audio": {
                "data": audition_clip(), "format": "wav",
                "source_path": place.join("missing.wav").to_string_lossy()
            }
        })];
        let prepared = store
            .prepare(
                &session.id,
                &incoming,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &facts(&project),
                    provider: "fixture",
                    model: "model",
                },
            )
            .unwrap();
        let loaded = Store::new(root.clone()).load(&session.id).unwrap();
        assert_eq!(prepared.saved_auditions, loaded.messages);
        assert!(prepared.saved_auditions[0].get("input_audio").is_none());
        assert_eq!(loaded.messages[0]["source_render_unavailable"], true);
        let excerpt = loaded.messages[0]["media"][0]["path"].as_str().unwrap();
        assert!(Path::new(excerpt).is_file());
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_repeated_audition_caption_keeps_distinct_audio_turns() {
        use base64::Engine as _;
        let (store, project) = fixture("same-audition-caption");
        let session = store.create(None, None, None, None).unwrap();
        let context = facts(&project);
        let audition = |sample| {
            let raw = oc_engine::wav::mono_wav_bytes(&[sample; 256], 16_000).unwrap();
            json!({
                "role": "user",
                "content": "You asked to hear this range.",
                "message_meta": {"version":1,"kind":"audition"}, "input_audio": {
                    "format": "wav",
                    "data": base64::engine::general_purpose::STANDARD.encode(raw),
                }
            })
        };
        for sample in [0.25, -0.25, -0.25] {
            store
                .prepare(
                    &session.id,
                    &[audition(sample)],
                    None,
                    crate::ai::sessions::PrepareContext {
                        snapshot: None,
                        overhead: 0,
                        context_length: 0,
                        facts: &context,
                        provider: "fixture",
                        model: "model",
                    },
                )
                .unwrap();
        }
        let messages = store.load(&session.id).unwrap().messages;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"], messages[1]["content"]);
        let media_path =
            |message: &Value| PathBuf::from(message["media"][0]["path"].as_str().unwrap());
        let first = media_path(&messages[0]);
        let second = media_path(&messages[1]);
        assert_ne!(first, second);
        store.collect_unreferenced().unwrap();
        assert!(first.is_file() && second.is_file());
    }

    #[test]
    fn archived_mp3_reuses_only_the_same_effective_encoding() {
        use crate::gui_settings::{SessionAudioFormat, SessionAudioSettings};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.wav");
        let _lock = Store::new(root.path().to_path_buf())
            .mutation_lock()
            .unwrap();
        let mut archive = ArchiveSweep::new(root.path());
        let mut encode = |bitrate| {
            archive
                .render(
                    &source,
                    &SessionAudioSettings {
                        format: SessionAudioFormat::Mp3,
                        mp3_kbps: bitrate,
                    },
                )
                .unwrap()
        };
        for rate in [48_000, 24_000] {
            let pcm = oc_engine::wav::pcm_wav_bytes(&[0.25; 8192], rate, 2).unwrap();
            std::fs::write(&source, pcm).unwrap();
            let compact = encode(128);
            let full = encode(256);
            assert_ne!(compact, full);
            assert_eq!(compact, encode(128));
            assert_eq!(full, encode(if rate == 48_000 { 265 } else { 320 }));
            assert_eq!(full.extension().unwrap(), "mp3");
            assert!(full.metadata().unwrap().len() > compact.metadata().unwrap().len());
            let bytes = std::fs::read(full).unwrap();
            let table = oc_engine::encode::mp3::Mp3Version::for_sample_rate(rate)
                .unwrap()
                .bitrates();
            let header_index = usize::from(bytes[2] >> 4);
            assert_eq!(
                table[header_index - 1],
                if rate == 48_000 { 256 } else { 160 }
            );
        }
    }

    #[test]
    fn a_raw_mp3_fallback_does_not_override_the_wav_archive() {
        use crate::gui_settings::{SessionAudioFormat, SessionAudioSettings};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("loud.wav");
        oc_engine::wav::write_stereo_f32_wav(&source, &[2.0; 8192], 48_000).unwrap();
        let _lock = Store::new(root.path().to_path_buf())
            .mutation_lock()
            .unwrap();
        let mut archive = ArchiveSweep::new(root.path());
        let raw = archive
            .render(
                &source,
                &SessionAudioSettings {
                    format: SessionAudioFormat::Mp3,
                    mp3_kbps: 192,
                },
            )
            .unwrap();
        assert_eq!(raw.extension().unwrap(), "wav");
        assert_eq!(
            std::fs::read(&raw).unwrap(),
            std::fs::read(&source).unwrap()
        );
        let wav = archive.render(&source, &Default::default()).unwrap();
        assert_ne!(raw, wav);
        assert_eq!(
            oc_engine::wav::open_reader(&wav)
                .unwrap()
                .spec()
                .sample_rate,
            44_100
        );
    }

    #[test]
    fn mp3_render_preference_preserves_the_exact_audition_for_reopened_turns() {
        for source_rate in [48_000, 96_000] {
            verify_reopened_mp3_audition(source_rate);
        }
    }

    fn verify_reopened_mp3_audition(source_rate: u32) {
        use crate::gui_settings::{SessionAudioFormat, SessionAudioSettings};
        use base64::Engine as _;
        let (store, project) = fixture(&format!("audition-container-{source_rate}"));
        let root = store.root().to_path_buf();
        let place = workspace(&project);
        let session = store
            .create(Some(&place), Some(&project), None, None)
            .unwrap();
        let settings = SessionAudioSettings {
            format: SessionAudioFormat::Mp3,
            mp3_kbps: 192,
        };

        let stereo: Vec<f32> = (0..24_000)
            .flat_map(|frame| [(frame as f32 * 0.05).sin() * 0.3, 0.1])
            .collect();
        let clip = oc_engine::wav::pcm_wav_bytes(&stereo, 24_000, 2).unwrap();
        let render = place.join("render.wav");
        let mut full_rate = oc_engine::wav::pcm_wav_bytes(&stereo, source_rate, 2).unwrap();
        add_padded_wav_metadata(&mut full_rate);
        std::fs::write(&render, &full_rate).unwrap();

        let mut messages = vec![json!({
            "role": "user",
            "content": "You asked to hear 0–1 s: this is that render.",
            "message_meta": {"version":1,"kind":"audition"}, "input_audio": {
                "data": base64::engine::general_purpose::STANDARD.encode(&clip),
                "format": "wav",
                "source_path": render.to_string_lossy(),
            }
        })];
        persist_auditions(&root, &mut messages, &settings).unwrap();
        let media_path = Path::new(messages[0]["media"][0]["path"].as_str().unwrap());
        assert_eq!(media_path.extension().unwrap(), "wav");
        assert_eq!(std::fs::read(media_path).unwrap(), clip);
        let archived_render = Path::new(messages[0]["source_render"].as_str().unwrap());
        assert_eq!(archived_render.extension().unwrap(), "mp3");
        let decoded = oc_engine::audio_clip::decode(archived_render).unwrap();
        assert_eq!(decoded.sample_rate, 48_000);
        assert_eq!(decoded.frames(), 24_000 * 48_000 / source_rate as usize);

        store
            .prepare(
                &session.id,
                &messages,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &facts(&project),
                    provider: "fixture",
                    model: "model",
                },
            )
            .unwrap();
        let loaded = Store::new(root.clone()).load(&session.id).unwrap();
        let mut request = ai::ProviderRequest {
            system: None,
            turns: ai::turns_from_wire(&Value::Array(loaded.messages)).unwrap(),
            tools: Vec::new(),
            max_tokens: 1024,
            temperature: 0.0,
            thinking_level: "medium".into(),
            stream: false,
        };
        assert!(ai::media::resolve(&mut request).unwrap().is_empty());
        let data = request
            .turns
            .iter()
            .flat_map(|turn| &turn.blocks)
            .find_map(|block| match block {
                ai::Block::Media { data, .. } => Some(data),
                _ => None,
            })
            .unwrap();
        let ai::media::MediaData::Inline { base64, .. } = data else {
            panic!("the reopened audition must resolve to inline audio");
        };
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(base64)
                .unwrap(),
            clip
        );
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn drop_history_keeps_the_chosen_compaction_and_collects_orphans() {
        let (store, project) = fixture("drop-history");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let dir = store.find(&session.id).unwrap().0;
        let keep_sha = "a".repeat(64);
        let drop_sha = "b".repeat(64);
        let first = Record {
            v: VERSION,
            kind: RecordKind::Turn,
            at: now(),
            dropped: None,
            tokens_before: None,
            tokens_after: None,
            truncated: None,
            messages: vec![json!({"role":"user","content":"old","dsl_sha":drop_sha})],
        };
        let compaction = Record {
            v: VERSION,
            kind: RecordKind::Compaction,
            at: now(),
            dropped: Some(Dropped { from: 0, to: 1 }),
            tokens_before: Some(10),
            tokens_after: Some(2),
            truncated: None,
            messages: vec![json!({"role":"user","content":"summary","dsl_sha":keep_sha})],
        };
        append_records(&dir, &[first, compaction]).unwrap();

        // One referenced asset and one orphan, in each content store.
        std::fs::create_dir_all(store.root().join("audition-media")).unwrap();
        std::fs::write(
            store
                .root()
                .join("audition-media")
                .join(format!("{keep_sha}.wav")),
            b"x",
        )
        .unwrap();
        std::fs::write(
            store
                .root()
                .join("audition-media")
                .join(format!("{drop_sha}.wav")),
            b"x",
        )
        .unwrap();
        let versions = dir.join("versions");
        std::fs::create_dir_all(&versions).unwrap();
        std::fs::write(versions.join(format!("{keep_sha}.ocp")), b"@project\n").unwrap();
        std::fs::write(versions.join(format!("{drop_sha}.ocp")), b"@project\n").unwrap();

        let outcome = store.drop_history(&session.id, 1).unwrap();
        assert_eq!(outcome["kept_records"], 1);
        assert!(outcome["removed_assets"].as_u64().unwrap() >= 2);
        assert!(
            store
                .root()
                .join("audition-media")
                .join(format!("{keep_sha}.wav"))
                .is_file()
        );
        assert!(
            !store
                .root()
                .join("audition-media")
                .join(format!("{drop_sha}.wav"))
                .is_file()
        );
        assert!(versions.join(format!("{keep_sha}.ocp")).is_file());
        assert!(!versions.join(format!("{drop_sha}.ocp")).is_file());
        // Asking for a compaction that does not exist is a refusal, not a wipe.
        assert!(store.drop_history(&session.id, 2).is_err());
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_dsl_snapshot_round_trips_and_refuses_a_foreign_id() {
        let (store, project) = fixture("dsl-snapshot");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let dsl = "@project\n  tempo: 120\n";
        let sha = store.snapshot_dsl(&session.id, dsl).unwrap();
        assert_eq!(store.dsl_version(&session.id, &sha).unwrap(), dsl);
        // The same document is one file, however many messages carry it.
        assert_eq!(store.snapshot_dsl(&session.id, dsl).unwrap(), sha);
        // A path-shaped or unknown id is refused rather than read.
        assert!(store.dsl_version(&session.id, "../../etc/passwd").is_err());
        assert!(store.dsl_version(&session.id, "not-a-hash").is_err());
        assert!(store.dsl_version(&session.id, &"0".repeat(64)).is_err());
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn a_provider_that_rejects_a_replayed_signature_accepts_the_compacted_conversation() {
        const REJECTION: &str = r#"{"error":{"message":"Gemini 4+ requires the original thought signature on the first tool call of each step in the current turn; preserve tool_calls[].extra_content.google.thought_signature.","type":"invalid_request_error"}}"#;
        const ANSWER: &str = r#"{"id":"chatcmpl-1","choices":[{"message":{"role":"assistant","content":"Bass is in."},"finish_reason":"stop"}]}"#;
        let (store, project) = fixture("accepts");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let context = facts(&project);
        let view = conversation();
        store
            .prepare(
                &session.id,
                &view,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        // A provider that refuses a replayed signature, exactly as the wire does.
        let (base, server) = canned_by_request(2, |request| {
            if request.contains("thought_signature") {
                (400, REJECTION.to_string())
            } else {
                (200, ANSWER.to_string())
            }
        });
        let entry = AiProviderEntry {
            id: "signature-fixture".into(),
            name: "Signature fixture".into(),
            kind: ProviderKind::OpenAiChatCompletions,
            wire: Some(Wire::OpenAiChatCompletions),
            auth: AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint: format!("{base}v1/chat/completions"),
            model: "google/gemini-4-argon".into(),
            models: Vec::new(),
            context_length: 32_000,
            max_tokens: 4_096,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: AiCapabilities {
                text: true,
                streaming: false,
                native_tool_calling: true,
                ..Default::default()
            },
        };
        let send = |messages: Value| async {
            let (request, _) = request_from_wire(&entry, messages, false, None).unwrap();
            let provider =
                provider_for(&entry, Credential::ApiKey(Some("fixture-secret".into()))).unwrap();
            let mut emitted = false;
            let stats = ai::timing::TurnStatsHandle::new(&entry.id, &entry.model, entry.wire());
            ai::run(provider.as_ref(), &request, &None, &mut emitted, &stats).await
        };
        // The saved conversation still replays its tool call with the signature,
        // so this provider refuses it.
        let error = send(json!(store.view(&session.id).unwrap()))
            .await
            .unwrap_err();
        assert!(error.message.contains("thought signature"), "{error:?}");

        let outcome = store.compact(&session.id, None, &context).unwrap();
        let answer = send(json!(outcome.messages)).await.unwrap();
        assert_eq!(answer["content"], "Bass is in.");
        let requests = server.join().unwrap();
        let messages = |request: &str| {
            let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
            let body: Value = serde_json::from_str(body).expect("the request body is JSON");
            body["messages"].to_string()
        };
        assert!(
            messages(&requests[0]).contains("thought_signature"),
            "the saved conversation replays the signature: {}",
            messages(&requests[0])
        );
        let compacted = messages(&requests[1]);
        for forbidden in [
            "thought_signature",
            "signature",
            "tool_calls",
            "provider_state",
        ] {
            assert!(
                !compacted.contains(forbidden),
                "the compacted request carried '{forbidden}': {compacted}"
            );
        }
        assert!(compacted.contains("Conversation summary"), "{compacted}");
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn the_token_estimate_is_monotone() {
        let mut view = Vec::new();
        let mut previous = 0;
        for message in conversation() {
            view.push(message);
            let tokens = estimate_tokens(&view);
            assert!(tokens > previous, "{tokens} !> {previous}");
            previous = tokens;
        }
        assert_eq!(
            estimate_tokens(&[user("abcd")]),
            estimate_tokens(&[user("abcd")]),
            "the estimate is a function of the messages"
        );
        // Non-ASCII text costs more per character than ASCII.
        assert!(estimate_tokens(&[user("一二三四")]) > estimate_tokens(&[user("abcd")]));
    }

    #[test]
    fn auto_compaction_runs_before_a_turn_over_the_threshold() {
        let (store, project) = fixture("auto");
        let session = store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                Some("long job"),
                None,
            )
            .unwrap();
        let context = facts(&project);
        let view = conversation();
        let before = estimate_tokens(&view);
        // A context window this conversation alone fills past the fraction.
        let window = before.max(1);
        let prepared = store
            .prepare(
                &session.id,
                &view,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: window,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        let compacted = prepared
            .compacted
            .expect("the turn compacts before sending");
        assert!(strips_replay_state(&prepared.messages));
        assert_eq!(prepared.messages, compacted.messages);
        assert!(compacted.tokens_after < compacted.tokens_before);
        assert!(prepared.meta.compacted_at.is_some());
        assert_eq!(prepared.meta.tokens, compacted.tokens_after);
        assert_eq!(
            prepared.messages.last().unwrap(),
            &expected_user("now the bass")
        );
        // A conversation under the threshold is left alone.
        let small = store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                Some("short"),
                None,
            )
            .unwrap();
        let prepared = store
            .prepare(
                &small.id,
                &[user("hello")],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 1_000_000,
                    facts: &context,
                    provider: "p",
                    model: "m",
                },
            )
            .unwrap();
        assert!(prepared.compacted.is_none());
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn an_oversized_session_stays_under_the_cap_and_says_what_it_dropped() {
        // The production cap is a gigabyte; the trimming path is exercised
        // against a small one so the test stays cheap.
        let (store, project) = fixture("cap");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let cap = 2 * 1024 * 1024u64;
        let filler = "x".repeat(100_000);
        let mut records = Vec::new();
        for index in 0..40 {
            records.push(Record {
                v: VERSION,
                kind: RecordKind::Turn,
                at: now(),
                dropped: None,
                tokens_before: None,
                tokens_after: None,
                truncated: None,
                messages: vec![
                    user(&format!("{index} {filler}")),
                    reply(&format!("{index} {filler}")),
                ],
            });
        }
        let dir = store.find(&session.id).unwrap().0;
        append_records_under(&dir, &records, cap).unwrap();
        let size = std::fs::metadata(dir.join(MESSAGES_FILE)).unwrap().len();
        assert!(size <= cap, "{size} bytes");
        let (view, _) = read_view(&dir).unwrap();
        let note = view[0]["content"].as_str().unwrap_or_default();
        assert!(note.contains("dropped"), "{note}");
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_recorded_path_re_associates_a_moved_workspace() {
        let (store, project) = fixture("moved");
        let session = store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                Some("before the move"),
                None,
            )
            .unwrap();
        let place = workspace(&project);
        let moved = oc_test_support::temp_dir("oc-ai-sessions", "moved-workspace");
        std::fs::rename(place.join("song.ocp"), moved.join("song.ocp")).unwrap();
        assert_ne!(
            store.workspace_key(Some(&moved)),
            store.workspace_key(Some(&place)),
            "a different directory has a different key"
        );
        let key = store.reassociate(&place, &moved).unwrap();
        assert!(
            store
                .root()
                .join(&key)
                .join(&session.id)
                .join(META_FILE)
                .is_file()
        );
        let listed = store.list(Scope::Workspace(Some(&moved))).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, session.id);
        let record: Value = serde_json::from_slice(
            &std::fs::read(store.root().join(&key).join(WORKSPACE_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(record["paths"].as_array().unwrap().len(), 2);
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_fork_copies_the_conversation_and_keeps_the_parent_id() {
        let (store, project) = fixture("fork");
        let parent = store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                Some("parent"),
                None,
            )
            .unwrap();
        let context = facts(&project);
        store
            .prepare(
                &parent.id,
                &[user("first")],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "p",
                    model: "m",
                },
            )
            .unwrap();
        store
            .record_reply(&parent.id, &reply("answer"), &context, "p", "m")
            .unwrap();
        let fork = store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                None,
                Some(Fork {
                    parent: &parent.id,
                    through: None,
                }),
            )
            .unwrap();
        assert_eq!(fork.parent.as_deref(), Some(parent.id.as_str()));
        assert_eq!(fork.title, "parent (fork)");
        assert_eq!(store.load(&fork.id).unwrap().messages.len(), 2);
        // The fork is independent: a new turn leaves the parent untouched.
        store
            .prepare(
                &fork.id,
                &[user("second")],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "p",
                    model: "m",
                },
            )
            .unwrap();
        assert_eq!(store.load(&parent.id).unwrap().messages.len(), 2);
        assert_eq!(store.load(&fork.id).unwrap().messages.len(), 3);
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_fork_cut_is_trimmed_to_an_answered_turn() {
        let mut messages = vec![
            user("first"),
            serde_json::json!({
                "role": "assistant",
                "content": "Trying a gain cut.",
                "tool_calls": [{ "id": "call-1", "type": "function", "function": { "name": "x", "arguments": "{}" } }],
            }),
        ];
        trim_unanswered_call(&mut messages);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");

        // An answer ends the turn, so nothing is trimmed.
        let mut answered = vec![user("first"), reply("Done.")];
        trim_unanswered_call(&mut answered);
        assert_eq!(answered.len(), 2);
    }

    #[test]
    fn a_fork_can_stop_at_a_chosen_turn() {
        let (store, project) = fixture("fork-through");
        let place = workspace(&project);
        let parent = store
            .create(Some(&place), Some(&project), Some("parent"), None)
            .unwrap();
        let context = facts(&project);
        for (question, answer) in [("first", "one"), ("second", "two")] {
            store
                .prepare(
                    &parent.id,
                    &[user(question)],
                    Some(&project),
                    crate::ai::sessions::PrepareContext {
                        snapshot: None,
                        overhead: 0,
                        context_length: 0,
                        facts: &context,
                        provider: "p",
                        model: "m",
                    },
                )
                .unwrap();
            store
                .record_reply(&parent.id, &reply(answer), &context, "p", "m")
                .unwrap();
        }
        assert_eq!(store.view(&parent.id).unwrap().len(), 4);

        let stopped = store
            .create(
                Some(&place),
                Some(&project),
                None,
                Some(Fork {
                    parent: &parent.id,
                    through: Some(2),
                }),
            )
            .unwrap();
        assert_eq!(stopped.parent.as_deref(), Some(parent.id.as_str()));
        assert_eq!(store.load(&stopped.id).unwrap().messages.len(), 2);

        // A cut that names no message is refused instead of copying everything,
        // and the refusal leaves no session behind.
        for through in [0, 5] {
            let refused = store.create(
                Some(&place),
                Some(&project),
                None,
                Some(Fork {
                    parent: &parent.id,
                    through: Some(through),
                }),
            );
            assert!(refused.is_err(), "through {through} was accepted");
        }
        assert_eq!(store.list(Scope::Workspace(Some(&place))).unwrap().len(), 2);
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_rejected_proposal_is_summarized_as_text_not_replayed() {
        let (store, project) = fixture("proposal");
        let place = workspace(&project);
        let session = store
            .create(Some(&place), Some(&project), Some("fill"), None)
            .unwrap();
        let context = facts(&project);
        let mut view = vec![
            user("add a fill before the chorus"),
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-edit",
                    "type": "function",
                    "function": {"name": "edit_subtree", "arguments": "{\"path\":\"track:kick\",\"new_dsl\":\"@track kick\"}"},
                    "extra_content": {"google": {"thought_signature": "sig-edit"}}
                }]
            }),
            json!({
                "role": "tool",
                "tool_call_id": "call-edit",
                "content": "{\"status\":\"pending_user_confirmation\",\"applied\":false}"
            }),
            user("actually never mind"),
        ];
        for step in 0..12 {
            view.push(user(&format!(
                "listen to bars {step} to {} and tell me how the low end sits against the pad",
                step + 2
            )));
            view.push(reply(&format!(
                "Bars {step}-{}: the kick and the bass overlap under 200 Hz, so the pad loses its                  bottom octave and the mix reads heavy on small speakers. A high-pass on the pad                  at 180 Hz and 1 dB less on the bass bus would clear it.",
                step + 2
            )));
        }
        store
            .prepare(
                &session.id,
                &view,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        let outcome = store.compact(&session.id, None, &context).unwrap();
        let text = outcome.messages[0]["content"].as_str().unwrap();
        assert!(
            text.contains("proposed a replacement for track:kick"),
            "{text}"
        );
        assert!(text.contains("not applied"), "{text}");
        assert!(strips_replay_state(&outcome.messages));
        let serialized = Value::Array(outcome.messages.clone()).to_string();
        for forbidden in ["tool_calls", "provider_state", "thought_signature"] {
            assert!(!serialized.contains(forbidden), "{forbidden}: {serialized}");
        }
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn restored_session_audio_keeps_latest_instruction_and_matching_tool_calls() {
        let (store, project) = fixture("restored-audio-prefix");
        let place = workspace(&project);
        let session = store
            .create(Some(&place), Some(&project), Some("listen"), None)
            .unwrap();
        let context = facts(&project);
        let prepare = |messages: &[Value]| {
            store
                .prepare(
                    &session.id,
                    messages,
                    Some(&project),
                    crate::ai::sessions::PrepareContext {
                        snapshot: None,
                        overhead: 0,
                        context_length: 0,
                        facts: &context,
                        provider: "fixture-provider",
                        model: "fixture-model",
                    },
                )
                .unwrap()
        };
        prepare(&[user("Original setup instruction")]);
        store
            .record_reply(
                &session.id,
                &json!({"role":"assistant","content":"Setup finished"}),
                &context,
                "fixture-provider",
                "fixture-model",
            )
            .unwrap();
        prepare(&[user("An earlier question")]);
        store
            .record_reply(
                &session.id,
                &json!({"role":"assistant","content":"Earlier answer"}),
                &context,
                "fixture-provider",
                "fixture-model",
            )
            .unwrap();
        let latest = expected_user("ONLY listen now; do not edit or rediscover instruments");
        prepare(std::slice::from_ref(&latest));
        let calls = json!({"role":"assistant","content":null,"tool_calls":[
            {"id":"audio-a","type":"function","function":{"name":"audition_range","arguments":"{\"start_bar\":1,\"end_bar\":9}"}},
            {"id":"audio-b","type":"function","function":{"name":"audition_range","arguments":"{\"start_bar\":9,\"end_bar\":17}"}}
        ]});
        store
            .record_reply(
                &session.id,
                &calls,
                &context,
                "fixture-provider",
                "fixture-model",
            )
            .unwrap();
        let incoming = vec![
            latest.clone(),
            calls,
            json!({"role":"tool","tool_call_id":"audio-a","content":"first rendered range"}),
            json!({"role":"tool","tool_call_id":"audio-b","content":"second rendered range"}),
            json!({"role":"user","content":"First clip", "message_meta": {"version":1,"kind":"audition"}, "input_audio":{"format":"wav","data":"UklGRg=="}}),
            json!({"role":"user","content":"Second clip", "message_meta": {"version":1,"kind":"audition"}, "input_audio":{"format":"wav","data":"UklGRg=="}}),
        ];
        let prepared = prepare(&incoming);
        let live = prepared.messages.clone();
        assert_eq!(live.len(), 10);
        assert_eq!(live[4], latest);
        assert_eq!(live[5]["tool_calls"][0]["id"], "audio-a");
        assert_eq!(live[6]["tool_call_id"], "audio-a");
        assert_eq!(live[7]["tool_call_id"], "audio-b");
        assert!(live[8]["media"][0]["path"].is_string());
        assert_eq!(live[8]["media"], live[9]["media"]);
        assert!(
            prepared
                .messages
                .iter()
                .all(|message| message.get("input_audio").is_none())
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn audition_audio_survives_session_reopen_compaction_and_reports_missing_files() {
        let (store, project) = fixture("audio");
        let place = workspace(&project);
        let session = store
            .create(Some(&place), Some(&project), Some("listen"), None)
            .unwrap();
        let context = facts(&project);
        let clip = json!({
            "role": "user",
            "content": "You asked to hear 0-8 s: this is that render.",
            "message_meta": {"version":1,"kind":"audition"}, "input_audio": {"data": "UklGRg==", "format": "wav"}
        });
        // A listening round, so the clip belongs to a retained tool block and
        // compaction keeps one audio row for it.
        let call = tool_call(
            "audition_range",
            "{\"start_bar\":1,\"end_bar\":9}",
            "sig-listen",
        );
        let result = tool_result("audition_range", &"rendered range ".repeat(200));
        let turn = vec![
            user("render a window and listen"),
            call,
            result,
            clip.clone(),
        ];
        let prepared = store
            .prepare(
                &session.id,
                &turn,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        assert!(prepared.live.is_none());
        assert!(
            prepared
                .messages
                .iter()
                .all(|message| message.get("input_audio").is_none())
        );
        let descriptor = prepared
            .messages
            .iter()
            .find_map(|message| message.get("media"))
            .unwrap()[0]
            .clone();
        let archived = prepared
            .messages
            .iter()
            .find(|message| message.get("media").is_some())
            .unwrap();
        assert_eq!(archived["context"], "audition");
        let audio_path = descriptor["path"].as_str().unwrap();

        assert_eq!(std::fs::read(audio_path).unwrap(), b"RIFF");
        let transcript =
            std::fs::read_to_string(store.find(&session.id).unwrap().0.join("messages.jsonl"))
                .unwrap();
        assert!(!transcript.contains("UklGRg=="));
        assert!(!transcript.contains("audio clip not saved"));
        assert!(prepared.compacted.is_none());
        let next = store
            .prepare(
                &session.id,
                &[user("what did you hear?")],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        assert!(next.live.is_none());
        assert!(
            next.messages
                .iter()
                .any(|message| message["media"][0] == descriptor)
        );
        let reopened = Store::new(store.root().to_owned());
        let loaded = reopened.load(&session.id).unwrap();
        assert!(
            loaded
                .messages
                .iter()
                .any(|message| message["media"][0] == descriptor)
        );
        reopened
            .record_reply(
                &session.id,
                &reply(&"heard ".repeat(2000)),
                &context,
                "p",
                "m",
            )
            .unwrap();
        reopened
            .record_reply(&session.id, &reply("Recent answer one"), &context, "p", "m")
            .unwrap();
        reopened
            .record_reply(&session.id, &reply("Recent answer two"), &context, "p", "m")
            .unwrap();
        let compacted = reopened.compact(&session.id, None, &context).unwrap();
        assert!(
            compacted
                .messages
                .iter()
                .any(|message| message["media"][0] == descriptor)
        );
        std::fs::remove_file(audio_path).unwrap();
        let missing = reopened.load(&session.id).unwrap();
        assert!(
            missing
                .messages
                .iter()
                .any(|message| message["media"][0]["missing"] == true)
        );

        assert!(
            next.messages
                .iter()
                .all(|message| message.get("input_audio").is_none())
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    /// A file the picker would accept, with bytes that need no decoder.
    fn attachable(dir: &Path, name: &str) -> Value {
        let path = dir.join(name);
        std::fs::write(&path, b"attachment fixture bytes").unwrap();
        serde_json::to_value(crate::ai::media::attach(&path.to_string_lossy()).unwrap()).unwrap()
    }

    #[test]
    fn a_user_message_round_trips_its_attachment_and_later_turns_replay_it() {
        let (store, project) = fixture("attachments");
        let place = workspace(&project);
        let session = store
            .create(Some(&place), Some(&project), Some("listen"), None)
            .unwrap();
        let context = facts(&project);
        let descriptor = attachable(place.as_path(), "take.wav");
        assert_eq!(descriptor["kind"], "audio");
        let first =
            json!({"role":"user","content":"what do you hear?","media":[descriptor.clone()]});
        let prepared = store
            .prepare(
                &session.id,
                std::slice::from_ref(&first),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        assert_eq!(prepared.appended, 1);

        // Store -> load returns the same seven fields, never the bytes.
        let loaded = store.load(&session.id).unwrap();
        let saved = &loaded.messages[0]["media"][0];
        for field in [
            "kind",
            "name",
            "path",
            "mime",
            "bytes",
            "duration_secs",
            "sha256",
        ] {
            assert_eq!(saved[field], descriptor[field], "{field}");
        }
        let raw = std::fs::read_to_string(
            store
                .root()
                .join(&session.workspace_key)
                .join(&session.id)
                .join(MESSAGES_FILE),
        )
        .unwrap();
        assert!(raw.contains("\"kind\":\"audio\""), "{raw}");
        assert!(!raw.contains("attachment fixture bytes"), "{raw}");

        // A later turn replays the saved message, descriptor included.
        let next = store
            .prepare(
                &session.id,
                &[user("and now?")],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        let replayed = next
            .messages
            .iter()
            .find(|message| message["content"] == "what do you hear?")
            .expect("the saved user message is replayed");
        assert_eq!(replayed["media"][0], descriptor);

        // The next turn's request re-reads the file: a changed clip is sent as
        // it is now and reported, and the untouched one is inlined verbatim.
        let replacement = b"a different take, edited on disk";
        std::fs::write(descriptor["path"].as_str().unwrap(), replacement).unwrap();
        let mut request = ai::ProviderRequest {
            system: None,
            turns: ai::turns_from_wire(&Value::Array(next.messages.clone())).unwrap(),
            tools: Vec::new(),
            max_tokens: 1024,
            temperature: 0.0,
            thinking_level: "medium".into(),
            stream: false,
        };
        let notices = ai::media::resolve(&mut request).unwrap();
        assert_eq!(notices.len(), 1);
        assert!(!notices[0].missing);
        let inline = request
            .turns
            .iter()
            .flat_map(|turn| &turn.blocks)
            .find_map(|block| match block {
                ai::Block::Media { data, .. } => Some(data),
                _ => None,
            })
            .expect("the replayed attachment is sent");
        let ai::media::MediaData::Inline { base64, .. } = inline else {
            panic!("the replayed attachment was not resolved");
        };
        use base64::Engine as _;
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(base64)
                .unwrap(),
            replacement.to_vec()
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_saved_attachment_for_a_deleted_file_loads_as_missing_and_is_not_sent() {
        let (store, project) = fixture("attachment-missing");
        let place = workspace(&project);
        let session = store
            .create(Some(&place), Some(&project), Some("listen"), None)
            .unwrap();
        let context = facts(&project);
        let descriptor = attachable(place.as_path(), "gone.wav");
        let path = descriptor["path"].as_str().unwrap().to_owned();
        let first = json!({"role":"user","content":"what do you hear?","media":[descriptor]});
        store
            .prepare(
                &session.id,
                &[first],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        std::fs::remove_file(path).unwrap();

        let loaded = store.load(&session.id).unwrap();
        assert_eq!(loaded.messages[0]["media"][0]["missing"], Value::Bool(true));

        // The next request keeps the text and drops the block instead of failing.
        let next = store
            .prepare(
                &session.id,
                &[user("and now?")],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        let mut request = ai::ProviderRequest {
            system: None,
            turns: ai::turns_from_wire(&Value::Array(next.messages)).unwrap(),
            tools: Vec::new(),
            max_tokens: 1024,
            temperature: 0.0,
            thinking_level: "medium".into(),
            stream: false,
        };
        let notices = ai::media::resolve(&mut request).unwrap();
        assert!(
            notices
                .iter()
                .any(|notice| notice.missing && notice.name == "gone.wav")
        );
        assert!(
            request
                .turns
                .iter()
                .flat_map(|turn| &turn.blocks)
                .all(|block| !matches!(block, ai::Block::Media { .. })),
            "the missing attachment was not sent"
        );
        assert!(
            request
                .turns
                .iter()
                .flat_map(|turn| &turn.blocks)
                .any(|block| {
                    matches!(block, ai::Block::Text(text) if text.contains("what do you hear?"))
                })
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_saved_descriptor_written_without_a_kind_is_repaired_on_read() {
        let (store, project) = fixture("legacy-media");
        let place = workspace(&project);
        let session = store
            .create(Some(&place), Some(&project), Some("listen"), None)
            .unwrap();
        let descriptor = attachable(place.as_path(), "take.wav");
        // An older build saved the six fields without the kind.
        let mut legacy = descriptor.clone();
        legacy.as_object_mut().unwrap().remove("kind");
        let message = json!({
            "role": "user",
            "content": "what do you hear?",
            "media": [legacy, json!({"name": "not an attachment"})],
        });
        let record = json!({
            "v": 1, "kind": "turn", "at": "2026-10-30T08:00:00.000Z", "messages": [message],
        });
        let dir = store.root().join(&session.workspace_key).join(&session.id);
        std::fs::write(dir.join(MESSAGES_FILE), format!("{}\n", record).as_bytes()).unwrap();

        // Reading repairs the kind from the MIME type and drops the entry that
        // cannot be a descriptor at all, so one bad record cannot fail a turn.
        let loaded = store.load(&session.id).unwrap();
        let media = loaded.messages[0]["media"].as_array().unwrap();
        assert_eq!(media.len(), 1, "{media:?}");
        assert_eq!(media[0]["kind"], "audio");
        assert_eq!(media[0]["path"], descriptor["path"]);

        // The repaired descriptor is what the next request replays.
        let prepared = store
            .prepare(
                &session.id,
                &[user("and now?")],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &facts(&project),
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        let turns = ai::turns_from_wire(&Value::Array(prepared.messages)).unwrap();
        assert!(
            turns
                .iter()
                .flat_map(|turn| &turn.blocks)
                .any(|block| matches!(block, ai::Block::Media { .. })),
            "the repaired attachment reaches the wire"
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    /// A window that reloads mid-turn stops the loop that owed the model its
    /// tool results. The next turn pays every unpaid call with one honest
    /// result, so the request never carries a call without an answer.
    #[test]
    fn a_round_the_window_left_unanswered_is_closed_before_the_next_turn() {
        let (store, project) = fixture("dangling-round");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let context = facts(&project);
        let system = json!({ "role": "system", "content": "you are the panel" });
        let turn = json!([system, user("make it punchy")]);
        store
            .prepare(
                &session.id,
                turn.as_array().unwrap(),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        // The provider answered with a call the window never got to run.
        let call = tool_call("tracks_mix", "{\"track\":\"kick\"}", "sig-0");
        store
            .record_reply(
                &session.id,
                &call,
                &context,
                "fixture-provider",
                "fixture-model",
            )
            .unwrap();
        assert_eq!(store.view(&session.id).unwrap().len(), 2);

        let next = json!([system, user("never mind, what changed?")]);
        let prepared = store
            .prepare(
                &session.id,
                next.as_array().unwrap(),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        let messages = &prepared.messages;
        let closed = &messages[messages.len() - 2];
        assert_eq!(closed["role"], "tool");
        assert_eq!(closed["tool_call_id"], "call-tracks_mix");
        assert!(
            closed["content"].as_str().unwrap().contains("Not run"),
            "the model reads why the call has no result: {closed}"
        );
        assert_eq!(
            messages.last().unwrap()["content"],
            "never mind, what changed?"
        );

        // The closing is saved once, so a later turn does not repeat it.
        let saved = store.view(&session.id).unwrap();
        assert_eq!(
            saved
                .iter()
                .filter(|message| message["role"] == "tool")
                .count(),
            1
        );
        let again = json!([system, user("and now?")]);
        let prepared = store
            .prepare(
                &session.id,
                again.as_array().unwrap(),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        let closings = prepared
            .messages
            .iter()
            .filter(|message| {
                message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("Not run"))
            })
            .count();
        assert_eq!(closings, 1);
    }

    /// A round whose results all arrived is history, not a debt: nothing is
    /// appended to it.
    #[test]
    fn a_round_that_got_its_results_is_left_alone() {
        let (store, project) = fixture("answered-round");
        let session = store
            .create(Some(&workspace(&project)), Some(&project), None, None)
            .unwrap();
        let context = facts(&project);
        let system = json!({ "role": "system", "content": "you are the panel" });
        let turn = json!([system, user("make it punchy")]);
        store
            .prepare(
                &session.id,
                turn.as_array().unwrap(),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        let call = tool_call("tracks_mix", "{\"track\":\"kick\"}", "sig-0");
        store
            .record_reply(
                &session.id,
                &call,
                &context,
                "fixture-provider",
                "fixture-model",
            )
            .unwrap();
        let round = json!([
            system,
            user("make it punchy"),
            call,
            tool_result("tracks_mix", "{\"ok\":true}")
        ]);
        let prepared = store
            .prepare(
                &session.id,
                round.as_array().unwrap(),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        assert_eq!(prepared.messages.len(), 3);

        let next = json!([system, user("now the bass")]);
        let prepared = store
            .prepare(
                &session.id,
                next.as_array().unwrap(),
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                },
            )
            .unwrap();
        assert!(
            !prepared.messages.iter().any(|message| {
                message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("Not run"))
            }),
            "an answered round keeps its own results: {prepared:?}",
            prepared = prepared.messages
        );
    }

    /// A restored conversation keeps the signed reasoning of its assistant
    /// turns, so the next round replays what the provider signed. A row saved
    /// before the field existed still loads and builds an unsigned turn.
    #[test]
    fn a_saved_thinking_block_survives_restore_and_legacy_rows_still_build() {
        let (store, project) = fixture("thinking-replay");
        let session = store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                Some("bass"),
                None,
            )
            .unwrap();
        let context = facts(&project);
        let signed = json!({
            "role": "assistant",
            "content": null,
            "thinking": [{"text": "Check the bass.", "signature": "sig-9"}],
            "tool_calls": [{
                "id": "call-bass",
                "type": "function",
                "function": {"name": "read_context", "arguments": "{\"path\":\"track:bass\"}"}
            }]
        });
        store
            .record_reply(
                &session.id,
                &signed,
                &context,
                "fixture-provider",
                "fixture-model",
            )
            .unwrap();

        let loaded = store.load(&session.id).unwrap();
        let saved = loaded
            .messages
            .iter()
            .find(|message| message["role"] == "assistant")
            .expect("the signed turn was saved");
        assert_eq!(
            saved["thinking"],
            json!([{"text": "Check the bass.", "signature": "sig-9"}])
        );
        let mut stripped = saved.clone();
        if let Value::Object(object) = &mut stripped {
            object.remove("thinking");
        }
        assert!(
            estimate_tokens(std::slice::from_ref(saved)) > estimate_tokens(&[stripped]),
            "the replayed reasoning counts toward the context estimate"
        );
        let turns = ai::turns_from_wire(&Value::Array(loaded.messages)).unwrap();
        assert!(
            turns
                .iter()
                .any(|turn| turn.blocks.iter().any(|block| matches!(
                    block,
                    ai::Block::Thinking { text, signature: Some(signature), .. }
                        if text.as_str() == "Check the bass." && signature == "sig-9"
                ))),
            "the restored turn keeps its signature: {turns:?}"
        );

        // A conversation saved by a build that had no `thinking` field.
        store
            .record_reply(
                &session.id,
                &reply("Legacy answer"),
                &context,
                "fixture-provider",
                "fixture-model",
            )
            .unwrap();
        let loaded = store.load(&session.id).unwrap();
        assert_eq!(loaded.skipped, 0);
        let turns = ai::turns_from_wire(&Value::Array(loaded.messages)).unwrap();
        assert!(turns.iter().any(|turn| {
            turn.blocks.iter().any(
                |block| matches!(block, ai::Block::Text(text) if text.as_str() == "Legacy answer"),
            )
        }));
        assert!(
            !turns.iter().any(|turn| {
                turn.blocks.iter().any(|block| {
                    matches!(
                        block,
                        ai::Block::Thinking {
                            signature: None,
                            redacted: None,
                            ..
                        }
                    )
                })
            }),
            "a legacy turn replays no unsigned reasoning block: {turns:?}"
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn new_notice_shaped_user_prose_stays_user_after_reopen() {
        let (store, project) = fixture("typed-user-prose");
        let session = store
            .create(None, Some(&project), Some("Goal"), None)
            .unwrap();
        let text = "Conversation summary written by the user.\nStanding intent: this is quoted text, not host metadata";
        store
            .prepare(
                &session.id,
                &[user(text)],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 1_000_000,
                    facts: &facts(&project),
                    provider: "fixture",
                    model: "model",
                },
            )
            .unwrap();
        let loaded = store.load(&session.id).unwrap();
        assert_eq!(loaded.messages[0]["message_meta"]["kind"], "user");
        assert_eq!(loaded.messages[0]["content"], text);
        let summary = summary_metadata(&loaded.messages, loaded.messages.len(), &[]);
        assert_eq!(
            summary.standing_intent.as_deref(),
            Some(collapse(text).as_str())
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn structured_summary_state_survives_nonstandard_prose_and_repeated_compaction() {
        let (store, project) = fixture("typed-summary-state");
        let mut row = json!({"role":"user", "context":"summary", "content":"Host summary body without English protocol headings"});
        let mut meta = MessageMeta::new(MessageKind::Summary);
        meta.standing_intent = Some("Finish the editable arrangement".into());
        meta.discarded_auditions = Some(23);
        meta.rollback_notice = Some("Host rollback outcome without a legacy prefix".into());
        message_protocol::tag(&mut row, meta);
        let view = vec![row, user("Continue")];
        let next_meta = summary_metadata(&view, view.len(), &[]);
        assert_eq!(
            next_meta.standing_intent.as_deref(),
            Some("Finish the editable arrangement")
        );
        assert_eq!(next_meta.discarded_auditions, Some(23));
        let body = summary_text(&view, view.len(), &[], &facts(&project), 1000, "now");
        assert!(body.contains("Host rollback outcome without a legacy prefix"));
        let mut next = json!({"role":"user","context":"summary","content":"Another display body"});
        message_protocol::tag(&mut next, next_meta.clone());
        assert_eq!(summary_metadata(&[next], 1, &[]), next_meta);
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn saved_message_metadata_preserves_replay_and_archive_fields() {
        let mut original = tool_call("project_get", "{}", "fixture-signed-thought");
        original["thinking"] =
            json!([{"text":"Exposed reasoning", "signature":"reasoning-signature"}]);
        let saved = saved_message(&original).unwrap();
        for key in ["tool_calls", "provider_state", "thinking"] {
            assert_eq!(saved[key], original[key]);
        }
        assert_eq!(saved["message_meta"]["kind"], "assistant");
        let raw = json!({"role":"user","context":"audition","content":"Listening delivery", "source_render":"/saved/render.wav"});
        let saved = saved_message(&raw).unwrap();
        assert_eq!(saved["source_render"], "/saved/render.wav");
        assert_eq!(saved["message_meta"]["kind"], "audition");
    }

    #[test]
    fn comparison_retention_uses_kinds_instead_of_english_audio_labels() {
        let rows = [MessageKind::ComparisonReference, MessageKind::ComparisonCandidate]
            .map(|kind| {
                let mut row = json!({"role":"user","context":"audition", "content":"A localized audio label", "media":[{}]});
                message_protocol::tag(&mut row,MessageMeta::new(kind));
                row
            });
        assert!(retained_comparison_pair(&rows).is_some());
        let mut swapped = rows.clone();
        swapped.swap(0, 1);
        assert!(retained_comparison_pair(&swapped).is_none());
    }

    #[test]
    fn standing_intent_keeps_a_users_reference_attachment_request() {
        let mut row = json!({"role":"user","content":"Recreate this song with editable instruments", "media":[{}]});
        message_protocol::tag_new(&mut row);
        assert_eq!(
            summary_metadata(&[row], 1, &[]).standing_intent.as_deref(),
            Some("Recreate this song with editable instruments")
        );
    }

    #[test]
    fn future_message_envelopes_roundtrip_normal_session_persistence() {
        let (store, project) = fixture("future-message-envelope");
        let session = store
            .create(None, Some(&project), Some("Future message"), None)
            .unwrap();
        let mut assistant = tool_call("project_get", "{}", "signed-future-tool");
        assistant["thinking"] =
            json!([{"text":"Exposed future thought", "signature":"signed-future-thought"}]);
        assistant["message_meta"] =
            json!({"version":9,"kind":"future_assistant","opaque":{"keep":[1,"verbatim"]}});
        let mut user = json!({"role":"user", "content":"Conversation summary written by a person", "context":"audition", "source_render":"/saved/reference.wav", "message_meta":{"version":9,"kind":"future_user","new_field":true}, "media":[{"kind":"audio","path":"/saved/reference.wav","name":"reference.wav","mime":"audio/wav","bytes":44,"duration_secs":1.0,"sha256":"a".repeat(64)}]});
        user["media"] = canonical_media(&user["media"]);
        let saved = [
            saved_message(&user).unwrap(),
            saved_message(&assistant).unwrap(),
        ];
        let dir = store.find(&session.id).unwrap().0;
        append_records(&dir, &[turn_record("now", saved.to_vec())]).unwrap();
        let (loaded, _) = read_view(&dir).unwrap();
        assert_eq!(loaded, saved);
        assert_eq!(loaded[0]["message_meta"], user["message_meta"]);
        assert_eq!(loaded[1]["message_meta"], assistant["message_meta"]);
        for key in ["tool_calls", "thinking", "provider_state"] {
            assert_eq!(loaded[1][key], assistant[key]);
        }
        assert_eq!(loaded[0]["media"], user["media"]);
        assert_eq!(loaded[0]["source_render"], user["source_render"]);
        assert_eq!(message_protocol::kind(&loaded[0]), Some(MessageKind::User));
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn new_inline_user_reference_keeps_its_standing_intent() {
        let (store, project) = fixture("new-user-inline-reference");
        let session = store
            .create(None, Some(&project), Some("Reference"), None)
            .unwrap();
        let raw = json!({"role":"user","content":"Recreate the attached reference song", "input_audio":{"data":audition_clip(),"format":"wav"}});
        let prepared = store
            .prepare(
                &session.id,
                &[raw],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 1_000_000,
                    facts: &facts(&project),
                    provider: "fixture",
                    model: "model",
                },
            )
            .unwrap();
        assert!(
            prepared.saved_auditions.is_empty(),
            "a user's reference is not a host listening delivery"
        );
        let loaded = store.load(&session.id).unwrap();
        assert_eq!(loaded.messages[0]["message_meta"]["kind"], "user");
        assert!(loaded.messages[0].get("context").is_none());
        assert_eq!(
            loaded.messages[0]["content"],
            "Recreate the attached reference song"
        );
        let path = loaded.messages[0]["media"][0]["path"].as_str().unwrap();
        assert!(Path::new(path).is_file(), "the reference audio stays saved");
        assert_eq!(
            summary_metadata(&loaded.messages, 1, &[])
                .standing_intent
                .as_deref(),
            Some("Recreate the attached reference song")
        );
        std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
    }

    #[test]
    fn repeated_user_reference_prose_keeps_distinct_audio_and_future_envelopes() {
        let mut first = expected_user("Use this reference");
        first["media"] = canonical_media(
            &json!([{"kind":"audio","name":"one.wav","path":"/one.wav","mime":"audio/wav","bytes":44,"sha256":"a".repeat(64)}]),
        );
        let mut second = first.clone();
        second["media"][0]["path"] = json!("/two.wav");
        assert!(!same_turn(&first, &second));
        assert!(same_turn(&first, &first.clone()));
        let mut future = first.clone();
        future["message_meta"] = json!({"version":9,"opaque":"first"});
        let mut changed = future.clone();
        changed["message_meta"]["opaque"] = json!("second");
        assert!(!same_turn(&future, &changed));
        assert!(same_turn(&future, &future.clone()));
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(
            rfc3339_from(UNIX_EPOCH),
            "1970-01-01T00:00:00.000Z".to_string()
        );
        assert_eq!(
            rfc3339_from(UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_250)),
            "2023-11-14T22:13:20.250Z"
        );
    }
}
