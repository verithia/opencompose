//! Saved AI conversations: the command surface, the project facts a summary
//! names, and the autosave hook the chat path calls on every turn.
//!
//! The commands reach the store at `~/.opencompose/ai/sessions` through
//! [`Store::config`]. Nothing here writes to the project.
use crate::ai::agent_loop::wire;
use crate::ai::host_notice::{BindCause, HostNotice};
use crate::ai::sessions::{
    self, BindingMismatch, BindingStatus, Compaction, Fork, Loaded, LoadedPage, MAX_PAGE_TURNS,
    PageRead, PrepareFailure, ProjectFacts, Scope, SessionError, SessionErrorKind, SessionMeta,
    Store,
};
use crate::ai_service::{AiError, AiErrorDetail};
use crate::commands::AiProviderEntry;
use crate::edit_history::HistoryMark;
use crate::process_lock::{LockError, ProcessLock};
use crate::state::OpenSessionLock;
use crate::state::{AppState, AppStateInner};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager, State};

/// Tauri event emitted when the autosave session changes.
pub const AUTO_SESSION_EVENT: &str = "ai_session";

fn store() -> Store {
    Store::config()
}

fn store_for(id: &str) -> Result<(std::path::PathBuf, SessionMeta), String> {
    store().find(id)
}

impl From<LockError> for SessionError {
    fn from(error: LockError) -> Self {
        if error.is_in_use() {
            Self::new(
                SessionErrorKind::InUse,
                "Another OpenCompose process has this conversation open.",
            )
        } else {
            Self::new(
                SessionErrorKind::SessionStore,
                format!("Cannot lock the conversation: {error}"),
            )
        }
    }
}

/// Take the process lock a turn holds on a conversation for as long as it runs.
pub(crate) fn hold_turn(store: &Store, id: &str) -> Result<ProcessLock, LockError> {
    store.acquire_lock(id)
}

/// Take the lock a conversation is opened under. The caller keeps the guard for
/// as long as the conversation is open.
fn acquire_session_lock(store: &Store, id: &str) -> Result<ProcessLock, SessionError> {
    hold_turn(store, id).map_err(SessionError::from)
}

/// Make `id` the panel's open conversation under a fresh hold. A conversation
/// this process already has open re-acquires in place; a failed acquisition
/// leaves the current one open.
fn hold_open_session(state: &AppState, store: &Store, id: &str) -> Result<(), SessionError> {
    let guard = acquire_session_lock(store, id)?;
    let mut inner = state.lock_inner();
    inner.session_lock = Some(OpenSessionLock {
        id: id.to_owned(),
        _guard: guard,
    });
    Ok(())
}

/// Release the panel's hold when it names `id`.
fn release_open_session(state: &AppState, id: &str) {
    let mut inner = state.lock_inner();
    if inner
        .session_lock
        .as_ref()
        .is_some_and(|open| open.id == id)
    {
        inner.session_lock = None;
    }
}

/// The project file a request is about: the given path, or the open one.
fn project_of(project: Option<&str>, inner: &AppStateInner) -> Option<PathBuf> {
    requested_project(project).or_else(|| inner.file_path.clone())
}

/// The project a request named itself; `None` when it named nothing.
fn requested_project(project: Option<&str>) -> Option<PathBuf> {
    project
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// The workspace of a project file: its directory. A request may name the
/// directory itself, so both forms work on the agent surface.
pub(crate) fn workspace_of(path: Option<&Path>) -> Option<PathBuf> {
    let path = path?;
    if path.is_dir() || path.to_string_lossy().ends_with(std::path::MAIN_SEPARATOR) {
        return Some(path.to_path_buf());
    }
    path.parent().map(Path::to_path_buf)
}

/// What a summary names instead of carrying the project: the path, the digest of
/// the DSL it was written against, and the track count.
pub fn facts(inner: &AppStateInner) -> ProjectFacts {
    ProjectFacts {
        path: inner
            .file_path
            .as_ref()
            .map(|path| path.display().to_string()),
        dsl_digest: sessions::dsl_digest(&inner.dsl_source),
        track_count: inner.project.tracks.len(),
    }
}

/// Sessions newest first: the current project's unless `all` is set.
#[tauri::command]
pub fn sessions_list(
    project: Option<String>,
    all: Option<bool>,
    state: State<AppState>,
) -> Result<Vec<SessionMeta>, String> {
    list_for(&state, project.as_deref(), all.unwrap_or(false))
}

/// List saved sessions for a project, or all projects.
pub fn list_for(
    state: &AppState,
    project: Option<&str>,
    all: bool,
) -> Result<Vec<SessionMeta>, String> {
    let requested = requested_project(project);
    let place = {
        let inner = state.lock_inner();
        crate::commands::workspace::scoped_workspace(&inner, requested.as_deref())
    };
    let store = store();
    if all {
        store.list(Scope::All)
    } else {
        store.list(Scope::Workspace(place.as_deref()))
    }
}

/// Starts a session and makes it the one the next turn saves into. `parent`
/// forks: the new session begins with a copy of that conversation, and
/// `through` copies only the first that many saved messages so the fork can
/// stop at a completed turn.
#[tauri::command]
pub fn sessions_new(
    project: Option<String>,
    title: Option<String>,
    parent: Option<String>,
    through: Option<usize>,
    state: State<AppState>,
) -> Result<SessionMeta, SessionError> {
    let meta = new_fork(
        &state,
        project.as_deref(),
        title.as_deref(),
        parent.as_deref(),
        through,
    )
    .map_err(SessionError::from)?;
    let store = store();
    hold_open_session(&state, &store, &meta.id)?;
    Ok(meta)
}

/// Binds a conversation to the project that is open now. The next turn carries
/// one typed notice for the model; this command writes no transcript row.
#[tauri::command]
pub fn sessions_bind(
    id: String,
    state: State<AppState>,
    requests: State<crate::ai_service::Requests>,
) -> Result<SessionMeta, SessionError> {
    let _lock = acquire_session_lock(&store(), &id)?;
    bind_if_idle(&state, &id, &requests).map_err(SessionError::from)
}

/// Bind a conversation to the open project. A turn that is still running owns
/// its conversation's binding, so a file opened meanwhile waits for it to end.
pub(crate) fn bind_if_idle(
    state: &AppState,
    id: &str,
    requests: &crate::ai_service::Requests,
) -> Result<SessionMeta, String> {
    requests
        .bind_unless_running(id, || bind_for(state, id))?
        .ok_or_else(|| {
            "This conversation is running a turn; wait for it to finish before binding it to \
             another file."
                .to_owned()
        })?
}

/// Bind a conversation to the open project without the request-slot check.
pub fn bind_for(state: &AppState, id: &str) -> Result<SessionMeta, String> {
    let open = state.lock_inner().file_path.clone();
    store().bind(id, open.as_deref(), BindCause::Chosen)
}

/// Whether a conversation works on the open project, compared canonically so
/// the panel never compares raw paths.
#[tauri::command]
pub fn sessions_binding(id: String, state: State<AppState>) -> Result<BindingStatus, String> {
    binding_for(&state, &id)
}

/// The binding status of a saved conversation.
pub fn binding_for(state: &AppState, id: &str) -> Result<BindingStatus, String> {
    let open = state.lock_inner().file_path.clone();
    store().binding_status(id, open.as_deref())
}

/// Create a session for a project, optionally forking a whole parent.
pub fn new_for(
    state: &AppState,
    project: Option<&str>,
    title: Option<&str>,
    parent: Option<&str>,
) -> Result<SessionMeta, String> {
    new_fork(state, project, title, parent, None)
}

/// Create a session, copying `through` messages of `parent` when a parent is
/// named. A message count without a parent names no conversation to cut.
pub fn new_fork(
    state: &AppState,
    project: Option<&str>,
    title: Option<&str>,
    parent: Option<&str>,
    through: Option<usize>,
) -> Result<SessionMeta, String> {
    let Some(parent) = parent else {
        if through.is_some() {
            return Err("a message count needs a session to fork from".to_owned());
        }
        return create_session(state, project, title, None);
    };
    create_session(state, project, title, Some(Fork { parent, through }))
}

fn create_session(
    state: &AppState,
    project: Option<&str>,
    title: Option<&str>,
    fork: Option<Fork<'_>>,
) -> Result<SessionMeta, String> {
    let (project, place) = {
        let inner = state.lock_inner();
        let requested = requested_project(project);
        let project = requested.clone().or_else(|| inner.file_path.clone());
        let place = crate::commands::workspace::scoped_workspace(&inner, requested.as_deref());
        (project, place)
    };
    let store = store();
    let meta = store.create(place.as_deref(), project.as_deref(), title, fork)?;
    Ok(meta)
}

/// The saved wire messages of one session, ready to replay.
#[tauri::command]
pub fn sessions_load(id: String) -> Result<Loaded, String> {
    store().load(&id)
}

/// The conversation at its newest turns, for the panel to open.
#[tauri::command]
pub fn sessions_open(
    id: String,
    turns: Option<usize>,
    state: State<AppState>,
) -> Result<LoadedPage, SessionError> {
    open_session_for(&state, &id, turns)
}

/// Open a conversation for the panel and keep its lock until another one opens.
pub(crate) fn open_session_for(
    state: &AppState,
    id: &str,
    turns: Option<usize>,
) -> Result<LoadedPage, SessionError> {
    let store = store();
    // The panel opens one conversation at a time. Taking the new lock first
    // keeps the current conversation open when another process holds this one.
    let guard = acquire_session_lock(&store, id)?;
    let page = store
        .open_page(id, turns.unwrap_or(MAX_PAGE_TURNS))
        .map_err(SessionError::from)?;
    state.lock_inner().session_lock = Some(OpenSessionLock {
        id: id.to_owned(),
        _guard: guard,
    });
    Ok(page)
}

/// The older turns before message `before`, refused as stale when the view
/// changed shape since `revision`.
#[tauri::command]
pub fn sessions_page(
    id: String,
    before: usize,
    turns: Option<usize>,
    revision: String,
) -> Result<PageRead, String> {
    if revision.len() > 256 {
        return Err("That page revision is not one this app wrote.".into());
    }
    store().page(&id, before, turns.unwrap_or(MAX_PAGE_TURNS), &revision)
}

/// Drop every record before this conversation's second-newest compaction, then
/// collect assets no remaining message refers to. A conversation with fewer
/// than three compactions keeps everything.
#[tauri::command]
pub fn sessions_clear_history(
    id: String,
    requests: State<crate::ai_service::Requests>,
) -> Result<sessions::ClearReport, SessionError> {
    let store = store();
    let _lock = acquire_session_lock(&store, &id)?;
    store.clear_old_history_guarded(&id, &|| refuse_while_running(&requests, &id))
}

/// Drop old history for every conversation, skipping the ones a turn is
/// writing. Preferences offers this as one action with one confirmation.
#[tauri::command]
pub fn sessions_clear_all_history(
    requests: State<crate::ai_service::Requests>,
) -> Result<sessions::ClearReport, String> {
    store().clear_all_old_history(&|id| requests.session_busy(id).unwrap_or(true))
}

/// A conversation a turn is writing keeps its shape until the turn ends.
///
/// The check runs as the store's guard, under the same mutation lock every
/// write of a turn takes, preparation first. A turn is registered as running
/// before its preparation takes that lock and stays registered until after
/// its last write. So either the turn is registered when the guard runs and
/// the command is refused, or the turn's preparation waits for the lock and
/// then reads the conversation the command left; no turn writes into a
/// conversation reshaped between a check and the command's write.
pub(crate) fn refuse_while_running(
    requests: &crate::ai_service::Requests,
    id: &str,
) -> Result<(), SessionError> {
    if requests.session_busy(id).unwrap_or(true) {
        return Err(SessionError::new(
            SessionErrorKind::Busy,
            "A turn is running in this conversation; stop it or wait for it to finish.",
        ));
    }
    Ok(())
}

/// Save the project DSL as it stands before a message is sent, so that message
/// can be rolled back to later.
#[tauri::command]
pub fn sessions_snapshot(id: String, dsl: String) -> Result<Value, String> {
    let sha = store().snapshot_dsl(&id, &dsl)?;
    Ok(json!({ "sha": sha, "bytes": dsl.len() }))
}

/// Read one saved DSL version back.
#[tauri::command]
pub fn sessions_version(id: String, sha: String) -> Result<Value, String> {
    let dsl = store().dsl_version(&id, &sha)?;
    Ok(json!({ "dsl": dsl }))
}

/// The history a compaction replaced, for the panel's original view.
#[tauri::command]
pub fn sessions_original(id: String) -> Result<Value, String> {
    let (messages, compactions) = store().original(&id)?;
    Ok(json!({ "messages": messages, "compactions": compactions }))
}

#[tauri::command]
/// Rename one saved session.
pub fn sessions_rename(id: String, title: String) -> Result<SessionMeta, String> {
    store().rename(&id, &title)
}

/// Sets what this session may do to the DSL without asking. The boundary takes
/// the enum, so a value outside the three modes is refused by deserialization.
#[tauri::command]
pub fn sessions_set_permission(
    id: String,
    permission: sessions::SessionPermission,
) -> Result<SessionMeta, String> {
    store().set_permission(&id, permission)
}

/// Sets or clears this conversation's goal. `None` clears it. The change is
/// one typed row, so it rewinds with the conversation; a change during an open
/// round waits for the next admitted turn.
#[tauri::command]
pub fn sessions_set_goal(
    id: String,
    text: Option<String>,
    requests: State<crate::ai_service::Requests>,
) -> Result<SessionMeta, String> {
    let running = requests.session_busy(&id).unwrap_or(true);
    store().set_goal(&id, text.as_deref(), running)
}

#[tauri::command]
/// Delete one saved session. The conversation's lock is required, so another
/// process cannot delete a conversation the panel has open.
pub fn sessions_delete(
    id: String,
    state: State<AppState>,
    requests: State<crate::ai_service::Requests>,
) -> Result<(), SessionError> {
    delete_session_for(&state, &requests, &id)
}

/// Delete a conversation after taking its lock; also releases the panel's hold
/// when the deleted conversation is the open one.
pub fn delete_session_for(
    state: &AppState,
    requests: &crate::ai_service::Requests,
    id: &str,
) -> Result<(), SessionError> {
    let store = store();
    let _lock = acquire_session_lock(&store, id)?;
    store.delete_guarded(id, &|| refuse_while_running(requests, id))?;
    release_open_session(state, id);
    Ok(())
}

/// Folds the older turns of a session into one plain-text summary. A refusal
/// is `{kind, message}`: `busy` while a turn runs, `conflict` when another
/// window compacted first.
#[tauri::command]
pub fn sessions_compact(
    id: String,
    target_tokens: Option<u64>,
    state: State<AppState>,
    requests: State<crate::ai_service::Requests>,
) -> Result<Compaction, SessionError> {
    compact_checked(&state, &requests, &id, target_tokens)
}

/// Compact a session to a target token budget.
pub fn compact_for(
    state: &AppState,
    requests: &crate::ai_service::Requests,
    id: &str,
    target_tokens: Option<u64>,
) -> Result<Compaction, String> {
    Ok(compact_checked(state, requests, id, target_tokens)?)
}

fn compact_checked(
    state: &AppState,
    requests: &crate::ai_service::Requests,
    id: &str,
    target_tokens: Option<u64>,
) -> Result<Compaction, SessionError> {
    let facts = {
        let inner = state.lock_inner();
        facts(&inner)
    };
    store().compact_requested(id, target_tokens, &facts, &|| {
        refuse_while_running(requests, id)
    })
}

/// One model turn's saved-view decision.
#[derive(Debug)]
pub struct PreparedTurn {
    pub session_id: String,
    pub messages: Value,
    pub compacted: Option<Compaction>,
    /// Archive files this turn's auditions evicted; the panel reports them.
    pub archive_evicted: Vec<crate::ai::sessions::EvictedAudition>,
    pub saved_auditions: Vec<Value>,
}

/// Identity captured by the request owner, not the active panel selection.
pub struct SavedTurnBinding<'a> {
    pub id: &'a str,
    /// The client's own id for this turn, carried by the audition event.
    pub request_id: Option<&'a str>,
    pub incarnation: &'a str,
    pub project: Option<&'a Path>,
    /// Tokens of the tool list the request carries beside its messages
    /// ([`wire::offered_tool_tokens`]); 0 for a request without tools.
    pub tool_tokens: u64,
    /// The tools and permission this turn offers, compared with the last saved
    /// surface before the turn's message.
    pub surface: Option<crate::ai::host_notice::ToolSurface>,
    /// Stops preparation's summary request with the turn that owns it.
    pub cancel: Option<&'a crate::ai::agent_loop::Cancel>,
}

/// Why preparation produced no outbound request. Once admitted, the incoming
/// message stays saved even if its subsequent summary request is cancelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareTurnError {
    Cancelled,
    /// Even compacted, the request is larger than the model's window.
    ContextOverflow {
        tokens: u64,
        window: u64,
    },
    /// No cut brings the saved conversation under the request's row bound.
    Unreplayable,
    /// The conversation is bound to another project than the open one.
    Binding(BindingMismatch),
    /// The store refused or failed.
    Store(String),
}

impl PrepareTurnError {
    /// The stable code an `ai_turn` failure carries.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::ContextOverflow { .. } => "context_overflow",
            Self::Binding(_) => "session_binding",
            Self::Unreplayable | Self::Store(_) => "session_save",
        }
    }
}

impl std::fmt::Display for PrepareTurnError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("Request cancelled"),
            Self::ContextOverflow { tokens, window } => write!(
                formatter,
                "This message makes the request about {tokens} tokens, more than the model's \
                 {window}-token window even after compaction. Nothing was sent or saved; \
                 shorten the message or choose a model with a larger window."
            ),
            Self::Unreplayable => formatter.write_str("The saved conversation cannot be replayed"),
            Self::Binding(mismatch) => formatter.write_str(&mismatch.message()),
            Self::Store(message) => formatter.write_str(message),
        }
    }
}

impl From<String> for PrepareTurnError {
    fn from(message: String) -> Self {
        Self::Store(message)
    }
}

fn background_failure(error: impl std::fmt::Display) -> PrepareTurnError {
    PrepareTurnError::Store(format!("background task failed: {error}"))
}

impl From<PrepareTurnError> for AiError {
    fn from(error: PrepareTurnError) -> Self {
        let detail = match &error {
            PrepareTurnError::Binding(mismatch) => Some(AiErrorDetail::BindingMismatch {
                bound: mismatch.bound.clone(),
                open: mismatch.open.clone(),
            }),
            _ => None,
        };
        let mut error = AiError::new(error.kind(), error.to_string());
        error.detail = detail;
        error
    }
}

impl BindingStatus {
    /// The typed mismatch, when the conversation and the open project differ.
    pub fn mismatch(&self) -> Option<BindingMismatch> {
        (!self.matches).then(|| BindingMismatch {
            bound: self.bound.clone().unwrap_or_default(),
            open: self.open.clone().unwrap_or_default(),
        })
    }
}

impl From<BindingMismatch> for AiError {
    fn from(mismatch: BindingMismatch) -> Self {
        let mut error = AiError::new("session_binding", mismatch.message());
        error.detail = Some(AiErrorDetail::BindingMismatch {
            bound: mismatch.bound,
            open: mismatch.open,
        });
        error
    }
}

/// Rows a prepared request keeps under, below the wire's 100 so the host's
/// state line and one round of results still fit.
const PREPARED_WIRE_ROWS: usize = 96;

pub(crate) fn validate_saved_project(expected: &Path, actual: Option<&Path>) -> Result<(), String> {
    let expected = expected.canonicalize().map_err(|error| error.to_string())?;
    let actual = actual
        .ok_or("No project is open")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if expected != actual {
        return Err("The saved turn belongs to another project".into());
    }
    Ok(())
}

/// The system row that names the project the model is about to read, so a
/// historical tool success cannot be mistaken for current state.
fn authoritative_row(facts: &ProjectFacts) -> Value {
    json!({"role": "system", "content": format!(
        "Authoritative current project state at request time: DSL sha256 {}; track count {}. Historical tool successes may have been rolled back or superseded. Inspect current project when continuing edits; do not assume historical mutations remain applied.", facts.dsl_digest, facts.track_count
    )})
}

/// The request a saved conversation becomes: the client's system rows, then
/// the conversation, folded under the row bound and, when the window is
/// known, toward it. `Err` names why it cannot be sent; the caller decides
/// whether that refuses the turn.
fn fitted_request(
    system: &[Value],
    saved: &[Value],
    tokens: Option<wire::TokenBudget>,
) -> Result<Vec<Value>, PrepareTurnError> {
    let mut outbound = request_with_state(system, saved);
    let limits = wire::FoldLimits {
        rows: PREPARED_WIRE_ROWS,
        bytes: crate::ai::LIMIT,
        tokens,
    };
    if !limits.fits(&outbound) {
        let ask = wire::turn_ask(&outbound);
        let shape = wire::FoldLimits {
            tokens: None,
            ..limits
        };
        let folded = wire::fold_wire(&mut outbound, &limits, ask)
            .or_else(|| wire::fold_wire(&mut outbound, &shape, ask));
        if let Some(fold) = folded {
            log::warn!(
                "[ai] a saved conversation was folded to {} messages ({} rows replaced)",
                outbound.len(),
                fold.to - fold.from
            );
        }
    }
    if outbound.len() > PREPARED_WIRE_ROWS {
        return Err(PrepareTurnError::Unreplayable);
    }
    if let Some(tokens) = tokens {
        let measured = tokens.measure(&outbound);
        if measured > tokens.window {
            return Err(PrepareTurnError::ContextOverflow {
                tokens: measured,
                window: tokens.window,
            });
        }
    }
    Ok(outbound)
}

/// The system rows and the conversation, with the one derived goal-and-plan
/// row after them. The row is part of every request, so admission measures it.
fn request_with_state(system: &[Value], saved: &[Value]) -> Vec<Value> {
    let mut outbound = system.to_vec();
    outbound.extend_from_slice(saved);
    let (goal, plan) = crate::ai::session_state::current_state(&outbound);
    if let Some(row) = crate::ai::session_state::context_row(goal.as_ref(), plan.as_deref()) {
        crate::ai::session_state::insert_after_system(&mut outbound, row);
    }
    outbound
}

/// Resolves the active session for the open workspace (creating one on the first
/// turn), appends what the request adds, and returns what to send.
///
/// Whether the request can be sent is decided before anything is saved: a
/// refusal (too large for the model, not replayable) leaves the conversation,
/// its archive and its metadata exactly as they were.
pub async fn prepare_turn_for<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    provider: &AiProviderEntry,
    messages: &Value,
    binding: SavedTurnBinding<'_>,
) -> Result<PreparedTurn, PrepareTurnError> {
    let id = binding.id.to_owned();
    let cancel = binding.cancel;
    prepare_turn_using(app, provider, messages, binding, |input| {
        crate::ai_service::compaction_summary(app, provider.clone(), &id, input, cancel)
    })
    .await
}

/// Preparation with an explicit summary provider. Tests inject credentials at
/// this boundary while exercising the same admission and publication path.
pub(crate) async fn prepare_turn_using<R, F, Fut>(
    app: &tauri::AppHandle<R>,
    provider: &AiProviderEntry,
    messages: &Value,
    binding: SavedTurnBinding<'_>,
    summarize: F,
) -> Result<PreparedTurn, PrepareTurnError>
where
    R: tauri::Runtime,
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, sessions::CompactionFailure>>,
{
    if binding
        .cancel
        .is_some_and(crate::ai::agent_loop::Cancel::is_cancelled)
    {
        return Err(PrepareTurnError::Cancelled);
    }
    let state = app.state::<AppState>();
    let session_id = binding.id;
    let store = store();
    let find_id = session_id.to_owned();
    let (_, meta) = tauri::async_runtime::spawn_blocking(move || store_for(&find_id))
        .await
        .map_err(background_failure)??;
    let (project, place, facts, source, outside, history) = {
        let inner = state.lock_inner();
        if let Some(expected) = binding.project {
            validate_saved_project(expected, inner.file_path.as_deref())?;
        }
        let project = project_of(None, &inner);
        let place = crate::commands::workspace::scoped_workspace(&inner, None);
        let outside = outside_changes(&meta, project.as_deref(), &inner, session_id);
        (
            project,
            place,
            facts(&inner),
            inner.dsl_source.clone(),
            outside,
            crate::ai::sessions::HistoryAt::of(&inner),
        )
    };
    let key = store.workspace_key(place.as_deref());
    if meta.workspace_key != key {
        return Err("The requested conversation belongs to another workspace"
            .to_owned()
            .into());
    }
    let session_id = session_id.to_owned();
    let incoming = messages
        .as_array()
        .ok_or_else(|| "Invalid AI request messages".to_string())?;
    let mut system: Vec<Value> = incoming
        .iter()
        .filter(|message| message["role"] == "system")
        .cloned()
        .collect();
    // Every derived row is part of the request for admission, so the window is
    // measured over what is actually sent.
    system.insert(0, authoritative_row(&facts));
    // What a request carries besides the saved conversation counts against
    // the window too: the system prompt and the tool list. Attachments ride
    // in the conversation and are measured there.
    let overhead = sessions::estimate_tokens(&system) + binding.tool_tokens;
    let context = crate::ai::limits::context_length(provider);
    let max_output = crate::ai::limits::max_output(provider);
    let prompt_tokens = crate::ai::compaction_prompt::prompt_tokens();
    let tokens = wire::TokenBudget::new(context, binding.tool_tokens)
        .map(|budget| budget.for_compaction(max_output, prompt_tokens));
    let (store_id, store_incoming, store_project) =
        (session_id.clone(), incoming.to_vec(), project.clone());
    let (store_source, store_history, store_facts) = (source, history.clone(), facts.clone());
    let (store_provider, store_model) = (provider.id.clone(), provider.model.clone());
    let (store_outside, store_surface) = (outside, binding.surface.clone());
    let store_incarnation = binding.incarnation.to_owned();
    let admit_system = system.clone();
    let mut prepared = tauri::async_runtime::spawn_blocking(move || {
        Store::config()
            .prepare_admitted(
                &store_id,
                &store_incoming,
                store_project.as_deref(),
                crate::ai::sessions::PrepareContext {
                    snapshot: Some(&store_source),
                    history: store_history.as_ref(),
                    overhead,
                    context_length: context,
                    max_output,
                    prompt_tokens,
                    facts: &store_facts,
                    provider: &store_provider,
                    model: &store_model,
                    outside: store_outside,
                    surface: store_surface,
                },
                Some(&store_incarnation),
                |planned| fitted_request(&admit_system, planned, tokens).map(|_| ()),
            )
            .map_err(|failure| match failure {
                PrepareFailure::Binding(mismatch) => PrepareTurnError::Binding(mismatch),
                PrepareFailure::Admitted(error) => error,
                PrepareFailure::Store(message) => PrepareTurnError::Store(message),
            })
    })
    .await
    .map_err(background_failure)??;
    // The summary call is slow, so it runs with the store lock released. The
    // commit appends one compaction record only if the conversation still
    // matches the plan; a failure compacts mechanically instead.
    if let Some(plan) = prepared.pending_compaction.take() {
        let summary = summarize(plan.input.clone()).await;
        let (summary, provider_failure) = match summary {
            Ok(summary) => (Some(summary), None),
            Err(sessions::CompactionFailure::Cancelled) => return Err(PrepareTurnError::Cancelled),
            Err(failure) => (None, Some(failure)),
        };
        if binding
            .cancel
            .is_some_and(crate::ai::agent_loop::Cancel::is_cancelled)
        {
            return Err(PrepareTurnError::Cancelled);
        }
        let resolution = tauri::async_runtime::spawn_blocking(move || {
            Store::config().finish_compaction(plan, summary.as_deref())
        })
        .await
        .map_err(background_failure)?;
        if let Some(compaction) = resolution.compaction.as_ref() {
            prepared.apply(compaction);
        }
        prepared.compaction_error = provider_failure.or(resolution.failure);
    }
    if let Some(reason) = prepared
        .compaction_error
        .and_then(sessions::CompactionFailure::notice_reason)
    {
        let _ = app.emit(
            AUTO_SESSION_EVENT,
            compaction_failed_event(&session_id, reason),
        );
        if reason == sessions::CompactionFailure::Store {
            log::warn!("[ai] session {session_id} auto-compaction failed");
        }
    }
    let saved = match prepared.live {
        Some(live) => live,
        None => prepared.messages,
    };
    // Admission measured this same conversation before it was saved; what
    // was saved differs only by archive references, so a refusal here would
    // contradict it. The turn is sent as fitted, or unfitted if that fails.
    let mut outbound = fitted_request(&system, &saved, tokens).unwrap_or_else(|error| {
        log::warn!("[ai] session {session_id} was saved but does not fit: {error}");
        request_with_state(&system, &saved)
    });
    if crate::ai::media::audio_supported(provider) {
        sessions::describe_audition_renders(store.root(), &mut outbound);
    } else {
        for entry in outbound
            .iter_mut()
            .filter_map(|message| message["media"].as_array_mut())
            .flatten()
        {
            if entry["kind"] == "audio" {
                entry["local_reference"] = json!(true);
            }
        }
    }
    sessions::bound_media_replay(&mut outbound, incoming);
    let turn = PreparedTurn {
        session_id,
        messages: Value::Array(outbound),
        compacted: prepared.compacted,
        archive_evicted: prepared.evicted_auditions,
        saved_auditions: prepared.saved_auditions,
    };
    emit_preparation_events(app, &turn, provider, binding.request_id);
    Ok(turn)
}

/// The one place every surface hears what preparation did to the conversation:
/// a compaction, evicted archive files and the auditions this turn saved.
fn emit_preparation_events<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    turn: &PreparedTurn,
    provider: &AiProviderEntry,
    request_id: Option<&str>,
) {
    if let Some(compaction) = turn.compacted.as_ref() {
        let _ = app.emit(
            AUTO_SESSION_EVENT,
            compaction_event(turn, compaction, provider),
        );
    }
    if !turn.archive_evicted.is_empty() {
        let _ = app.emit(AUTO_SESSION_EVENT, archive_event(turn));
    }
    if !turn.saved_auditions.is_empty() {
        let mut event = json!({
            "session_id": turn.session_id,
            "saved_auditions": turn.saved_auditions,
        });
        if let Some(request_id) = request_id {
            event["request_id"] = json!(request_id);
        }
        let _ = app.emit(AUTO_SESSION_EVENT, event);
    }
}

/// The outside-changes notice to save before this turn. A pending binding
/// already tells the model about the file boundary, and a mark for another
/// project says nothing about this one.
fn outside_changes(
    meta: &SessionMeta,
    project: Option<&Path>,
    inner: &AppStateInner,
    session_id: &str,
) -> Option<HostNotice> {
    if meta.pending_binding.is_some() {
        return None;
    }
    let mark = meta.history_mark.as_ref()?;
    let open = sessions::canonical_path(project?)
        .to_string_lossy()
        .to_string();
    let saved = sessions::canonical_path(Path::new(&mark.project))
        .to_string_lossy()
        .to_string();
    if open != saved {
        return None;
    }
    let changes = inner.undo.since(
        &HistoryMark {
            at: mark.at,
            max_id: mark.max_id,
        },
        session_id,
        &inner.project.properties,
    )?;
    Some(HostNotice::OutsideChanges {
        entries: changes.entries,
        more: changes.more,
        undone: changes.undone,
        own_undone: changes.own_undone,
        rewritten: changes.rewritten,
        pruned: changes.pruned,
    })
}

/// Saves the assistant message a finished turn produced.
pub fn record_reply_for(
    app: &tauri::AppHandle,
    session_id: &str,
    reply: &Value,
    provider: &AiProviderEntry,
    incarnation: Option<&str>,
) -> Result<SessionMeta, String> {
    let state = app.state::<AppState>();
    let facts = {
        let inner = state.lock_inner();
        facts(&inner)
    };
    store().record_reply_owned(
        session_id,
        reply,
        &facts,
        (&provider.id, &provider.model),
        incarnation,
    )
}

/// The payload of [`AUTO_SESSION_EVENT`]: the panel adopts `messages` for the
/// next turn and can tell the user what was dropped.
pub fn compaction_event(
    turn: &PreparedTurn,
    compaction: &Compaction,
    provider: &AiProviderEntry,
) -> Value {
    json!({
        "session_id": turn.session_id,
        "compacted": true,
        "provider": provider.id,
        "model": provider.model,
        "dropped_from": compaction.dropped_from,
        "dropped_to": compaction.dropped_to,
        "tokens_before": compaction.tokens_before,
        "tokens_after": compaction.tokens_after,
        "retained_pairs": compaction.retained_pairs,
        "messages": turn.messages,
    })
}

/// The payload of [`AUTO_SESSION_EVENT`] after a mid-turn compaction landed.
pub fn compaction_recorded_event(session_id: &str, compaction: &Compaction) -> Value {
    json!({
        "session_id": session_id,
        "compacted": true,
        "dropped_from": compaction.dropped_from,
        "dropped_to": compaction.dropped_to,
        "tokens_before": compaction.tokens_before,
        "tokens_after": compaction.tokens_after,
        "retained_pairs": compaction.retained_pairs,
    })
}

/// The payload of [`AUTO_SESSION_EVENT`] when the summary call for a
/// compaction failed: the mechanical summary replaced it, and the panel says
/// why the model text is missing.
pub fn compaction_failed_event(session_id: &str, reason: sessions::CompactionFailure) -> Value {
    json!({
        "session_id": session_id,
        "notice": "compaction_failed",
        "reason": reason,
    })
}

/// The payload of [`AUTO_SESSION_EVENT`] when a turn's auditions had to evict
/// older archive files: the panel says what went so the loss is not silent.
pub fn archive_event(turn: &PreparedTurn) -> Value {
    json!({
        "session_id": turn.session_id,
        "evicted": turn.archive_evicted,
    })
}

#[cfg(test)]
mod fit_tests {
    use super::*;
    use crate::ai::sessions::turns;

    fn user(text: &str) -> Value {
        json!({"role":"user","content":text,"message_meta":{"version":1,"kind":"user"}})
    }

    fn summary() -> Value {
        json!({"role":"user","context":"summary","content":"Conversation summary written at noon.",
               "message_meta":{"version":1,"kind":"summary","standing_intent":"a longer song",
                               "discarded_auditions":0}})
    }

    fn text_round(saved: &mut Vec<Value>, round: usize) {
        let id = format!("text-call-t-{round}");
        saved.push(json!({"role":"assistant","content":"{\"tool\":\"tracks_list\",\"args\":{}}",
            "message_meta":{"version":1,"kind":"assistant","text_call":{"id":id,"tool":"tracks_list"}}}));
        saved.push(json!({"role":"user","content":format!("{}{{}}", turns::APPLICATION_RESULT_PREFIX),
            "message_meta":{"version":1,"kind":"application_result","text_call":{"id":id,"tool":"tracks_list"}}}));
    }

    #[test]
    fn a_long_request_keeps_the_summary_and_never_resumes_on_a_result() {
        let system = [json!({"role":"system","content":"prompt"})];
        let mut saved = vec![summary(), user("continue the arrangement")];
        for round in 0..60 {
            text_round(&mut saved, round);
        }
        let outbound = fitted_request(&system, &saved, None).unwrap();
        assert!(outbound.len() <= PREPARED_WIRE_ROWS);
        assert!(
            sessions::is_compaction_summary(&outbound[1]),
            "{}",
            outbound[1]
        );
        assert_eq!(outbound[2]["content"], "continue the arrangement");
        assert_eq!(turns::check_pairing(&outbound[1..]), Ok(()));
    }

    #[test]
    fn the_authoritative_row_is_measured_before_admission() {
        let facts = ProjectFacts {
            path: None,
            dsl_digest: "a".repeat(16),
            track_count: 1,
        };
        let system = vec![
            authoritative_row(&facts),
            json!({"role":"system","content":"prompt"}),
        ];
        let saved = [user("continue")];
        let budget = wire::TokenBudget::new(1_000_000, 0).unwrap();
        let mut without_row = vec![system[1].clone()];
        without_row.extend(saved.iter().cloned());
        let window = budget.measure(&without_row) + 1;
        let error = fitted_request(&system, &saved, wire::TokenBudget::new(window, 0)).unwrap_err();
        assert!(matches!(error, PrepareTurnError::ContextOverflow { .. }));
        // The measured system is the one the caller sends.
        let outbound = fitted_request(&system, &saved, None).unwrap();
        assert!(outbound.iter().any(|row| row == &system[0]));
    }

    #[test]
    fn the_goal_and_plan_row_is_measured_before_admission() {
        use crate::ai::session_state::{SessionState, StateAuthor, state_row};
        let system = [json!({"role":"system","content":"prompt"})];
        let saved = [
            state_row(&SessionState::GoalSet {
                text: "finish the bridge".into(),
                author: StateAuthor::Person,
            }),
            user("continue"),
        ];
        let budget = wire::TokenBudget::new(1_000_000, 0).unwrap();
        let mut without_row = system.to_vec();
        without_row.extend(saved.iter().cloned());
        // One token more than the conversation alone needs leaves room for the
        // derived row only if admission forgets to count it.
        let window = budget.measure(&without_row) + 1;
        let error = fitted_request(&system, &saved, wire::TokenBudget::new(window, 0)).unwrap_err();
        assert!(matches!(error, PrepareTurnError::ContextOverflow { .. }));
        let outbound = fitted_request(&system, &saved, None).unwrap();
        assert!(
            outbound
                .iter()
                .any(|row| row["context"] == crate::ai::session_state::CONTEXT_STATE),
            "the row the model reads is part of the request"
        );
    }

    #[test]
    fn a_request_larger_than_the_window_is_refused_as_context_overflow() {
        let system = [json!({"role":"system","content":"prompt"})];
        let saved = [user(&"pasted text ".repeat(40_000))];
        let tokens = wire::TokenBudget::new(64_000, 0);
        let error = fitted_request(&system, &saved, tokens).unwrap_err();
        assert!(matches!(
            error,
            PrepareTurnError::ContextOverflow { window: 64_000, .. }
        ));
        assert_eq!(AiError::from(error).kind, "context_overflow");
        assert!(fitted_request(&system, &saved[..], wire::TokenBudget::new(1_000_000, 0)).is_ok());
    }

    #[test]
    fn a_request_no_cut_can_shorten_is_unreplayable() {
        let system = [json!({"role":"system","content":"prompt"})];
        let ids: Vec<String> = (0..100).map(|call| format!("c{call}")).collect();
        let mut saved = vec![
            user("run them all"),
            json!({"role":"assistant","content":null,"message_meta":{"version":1,"kind":"assistant"},
                "tool_calls": ids.iter().map(|id| json!({"id":id,"type":"function",
                    "function":{"name":"tracks_list","arguments":"{}"}})).collect::<Vec<_>>()}),
        ];
        saved.extend(ids.iter().map(|id| {
            json!({"role":"tool","tool_call_id":id,"content":"{}","message_meta":{"version":1,"kind":"tool"}})
        }));
        let error = fitted_request(&system, &saved, None).unwrap_err();
        assert_eq!(error, PrepareTurnError::Unreplayable);
        assert_eq!(error.kind(), "session_save");
    }
}
