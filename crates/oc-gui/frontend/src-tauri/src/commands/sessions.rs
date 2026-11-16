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
use crate::state::{AppState, AppStateInner};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager, State};

/// Tauri event emitted when the autosave session changes.
pub const AUTO_SESSION_EVENT: &str = "ai_session";

fn store() -> Store {
    Store::config()
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
) -> Result<SessionMeta, String> {
    new_fork(
        &state,
        project.as_deref(),
        title.as_deref(),
        parent.as_deref(),
        through,
    )
}

/// Binds a conversation to the project that is open now. The next turn carries
/// one typed notice for the model; this command writes no transcript row.
#[tauri::command]
pub fn sessions_bind(id: String, state: State<AppState>) -> Result<SessionMeta, String> {
    bind_for(&state, &id)
}

/// Bind a conversation to the open project.
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
pub fn sessions_open(id: String, turns: Option<usize>) -> Result<LoadedPage, String> {
    store().open_page(&id, turns.unwrap_or(MAX_PAGE_TURNS))
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

/// Drop all history before the `keep`-th compaction, then collect assets no
/// remaining message refers to.
#[tauri::command]
pub fn sessions_drop_history(id: String, keep: u64) -> Result<Value, String> {
    Ok(store().drop_history_guarded(&id, keep, &|| refuse_while_running(&id))?)
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
fn refuse_while_running(id: &str) -> Result<(), SessionError> {
    if crate::ai::agent_loop::host::session_running(id) {
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

#[tauri::command]
/// Delete one saved session.
pub fn sessions_delete(id: String) -> Result<(), String> {
    Ok(store().delete_guarded(&id, &|| refuse_while_running(&id))?)
}

/// Folds the older turns of a session into one plain-text summary. A refusal
/// is `{kind, message}`: `busy` while a turn runs, `conflict` when another
/// window compacted first.
#[tauri::command]
pub fn sessions_compact(
    id: String,
    target_tokens: Option<u64>,
    state: State<AppState>,
) -> Result<Compaction, SessionError> {
    compact_checked(&state, &id, target_tokens)
}

/// Compact a session to a target token budget.
pub fn compact_for(
    state: &AppState,
    id: &str,
    target_tokens: Option<u64>,
) -> Result<Compaction, String> {
    Ok(compact_checked(state, id, target_tokens)?)
}

fn compact_checked(
    state: &AppState,
    id: &str,
    target_tokens: Option<u64>,
) -> Result<Compaction, SessionError> {
    let facts = {
        let inner = state.lock_inner();
        facts(&inner)
    };
    store().compact_requested(id, target_tokens, &facts, &|| refuse_while_running(id))
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
    pub incarnation: &'a str,
    pub project: Option<&'a Path>,
    /// Tokens of the tool list the request carries beside its messages
    /// ([`wire::offered_tool_tokens`]); 0 for a request without tools.
    pub tool_tokens: u64,
    /// The tools and permission this turn offers, compared with the last saved
    /// surface before the turn's message.
    pub surface: Option<crate::ai::host_notice::ToolSurface>,
}

/// Why a turn was not prepared. Every variant leaves the store unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareTurnError {
    /// Even compacted, the request is larger than the model's window.
    ContextOverflow { tokens: u64, window: u64 },
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
            Self::ContextOverflow { .. } => "context_overflow",
            Self::Binding(_) => "session_binding",
            Self::Unreplayable | Self::Store(_) => "session_save",
        }
    }
}

impl std::fmt::Display for PrepareTurnError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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

/// The request a saved conversation becomes: the client's system rows, then
/// the conversation, folded under the row bound and, when the window is
/// known, toward it. `Err` names why it cannot be sent; the caller decides
/// whether that refuses the turn.
fn fitted_request(
    system: &[Value],
    saved: &[Value],
    tokens: Option<wire::TokenBudget>,
) -> Result<Vec<Value>, PrepareTurnError> {
    let mut outbound = system.to_vec();
    outbound.extend_from_slice(saved);
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

/// Resolves the active session for the open workspace (creating one on the first
/// turn), appends what the request adds, and returns what to send.
///
/// Whether the request can be sent is decided before anything is saved: a
/// refusal (too large for the model, not replayable) leaves the conversation,
/// its archive and its metadata exactly as they were.
pub fn prepare_turn_for<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    provider: &AiProviderEntry,
    messages: &Value,
    binding: SavedTurnBinding<'_>,
) -> Result<PreparedTurn, PrepareTurnError> {
    let state = app.state::<AppState>();
    let session_id = binding.id;
    let store = store();
    let (_, meta) = store.find(session_id)?;
    let (project, place, facts, source, outside) = {
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
    let system: Vec<Value> = incoming
        .iter()
        .filter(|message| message["role"] == "system")
        .cloned()
        .collect();
    // What a request carries besides the saved conversation counts against
    // the window too: the system prompt and the tool list. Attachments ride
    // in the conversation and are measured there.
    let overhead = sessions::estimate_tokens(&system) + binding.tool_tokens;
    let context = crate::ai::limits::context_length(provider);
    let tokens = wire::TokenBudget::new(context, binding.tool_tokens);
    let prepared = store
        .prepare_admitted(
            &session_id,
            incoming,
            project.as_deref(),
            crate::ai::sessions::PrepareContext {
                snapshot: Some(&source),
                overhead,
                context_length: context,
                facts: &facts,
                provider: &provider.id,
                model: &provider.model,
                outside,
                surface: binding.surface,
            },
            Some(binding.incarnation),
            |planned| fitted_request(&system, planned, tokens).map(|_| ()),
        )
        .map_err(|failure| match failure {
            PrepareFailure::Binding(mismatch) => PrepareTurnError::Binding(mismatch),
            PrepareFailure::Admitted(error) => error,
            PrepareFailure::Store(message) => PrepareTurnError::Store(message),
        })?;
    if let Some(error) = prepared.compaction_error.as_ref() {
        let _ = app.emit(
            AUTO_SESSION_EVENT,
            compaction_failed_event(&session_id, error),
        );
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
        let mut outbound = system.clone();
        outbound.extend(saved);
        outbound
    });
    outbound.insert(0, json!({"role": "system", "content": format!(
        "Authoritative current project state at request time: DSL sha256 {}; track count {}. Historical tool successes may have been rolled back or superseded. Inspect current project when continuing edits; do not assume historical mutations remain applied.", facts.dsl_digest, facts.track_count
    )}));
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
    Ok(PreparedTurn {
        session_id,
        messages: Value::Array(outbound),
        compacted: prepared.compacted,
        archive_evicted: prepared.evicted_auditions,
        saved_auditions: prepared.saved_auditions,
    })
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
        rewritten: changes.rewritten,
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

/// The payload of [`AUTO_SESSION_EVENT`] when the compaction a turn needed
/// failed: the turn went out uncompacted, and the panel can say so.
pub fn compaction_failed_event(session_id: &str, error: &str) -> Value {
    json!({
        "session_id": session_id,
        "notice": "compaction_failed",
        "message": error,
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
